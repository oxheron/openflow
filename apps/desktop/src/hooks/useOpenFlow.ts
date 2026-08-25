import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { DesktopSettings } from "../domain/settings";
import {
  dictationReducer,
  initialDictationState,
  renderedTranscript,
  type DictationAction,
  type DictationState,
} from "../domain/session";
import type {
  ConnectionState,
  CorrectionPatch,
  ModelSpec,
  ServerCapabilities,
  ServerEvent,
} from "../domain/protocol";
import { TargetTracker } from "../domain/targetTracking";
import { PcmAudioCapture, SpeechBoundaryDetector, type AudioCaptureDiagnostics } from "../lib/audioCapture";
import {
  applyNativePatch,
  captureTarget,
  getPlatformCapabilities,
  insertStableText,
  isOpenFlowWindowFocused,
  releaseTarget,
  type PlatformCapabilities,
  type TargetLease,
} from "../lib/bridge";
import { friendlyError, isTauriRuntime, randomUuid, sha256Hex } from "../lib/runtime";
import { OpenFlowServerClient } from "../lib/serverClient";

export interface OpenFlowController {
  connection: ConnectionState;
  connectionError: string | null;
  capabilities: ServerCapabilities | null;
  platform: PlatformCapabilities | null;
  deliveryPolicy: TargetLease["policy"] | null;
  deliveryReason: string | null;
  models: ModelSpec[];
  session: DictationState;
  transcript: string;
  audioDiagnostics: AudioCaptureDiagnostics | null;
  connect: () => Promise<void>;
  disconnect: () => void;
  toggle: () => Promise<void>;
  start: () => Promise<void>;
  stop: () => Promise<void>;
  refreshModels: () => Promise<void>;
  downloadModel: (id: string) => Promise<void>;
  cancelModelDownload: (id: string) => Promise<void>;
  activateModel: (id: string) => Promise<void>;
  deactivateModel: (id: string) => Promise<void>;
  deleteModel: (id: string) => Promise<void>;
  pair: (pairingCode: string, deviceName: string) => Promise<string>;
  pairInteractively: (deviceName: string, verificationCode: string) => Promise<string>;
}

export function useOpenFlow(settings: DesktopSettings): OpenFlowController {
  const [connection, setConnection] = useState<ConnectionState>("disconnected");
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [capabilities, setCapabilities] = useState<ServerCapabilities | null>(null);
  const [models, setModels] = useState<ModelSpec[]>([]);
  const [platform, setPlatform] = useState<PlatformCapabilities | null>(null);
  const [deliveryPolicy, setDeliveryPolicy] = useState<TargetLease["policy"] | null>(null);
  const [deliveryReason, setDeliveryReason] = useState<string | null>(null);
  const [audioDiagnostics, setAudioDiagnostics] = useState<AudioCaptureDiagnostics | null>(null);
  const [session, dispatch] = useReducer(dictationReducer, initialDictationState);
  const sessionRef = useRef(session);
  const modelsRef = useRef(models);
  const clientRef = useRef<OpenFlowServerClient | null>(null);
  const audioRef = useRef(new PcmAudioCapture());
  const targetRef = useRef<TargetLease | null>(null);
  const trackerRef = useRef(new TargetTracker());
  const nativeRevisionRef = useRef(0);
  const writerQueueRef = useRef(Promise.resolve());
  const boundaryRef = useRef(new SpeechBoundaryDetector());
  const automaticDownloadAttemptsRef = useRef(new Set<string>());
  const automaticActivationAttemptsRef = useRef(new Set<string>());
  const startGenerationRef = useRef(0);
  const startControlSentRef = useRef(false);
  const startAcknowledgementRef = useRef<{
    sessionId: string;
    timer: number;
    resolve: () => void;
    reject: (error: Error) => void;
  } | null>(null);

  const emit = useCallback((action: DictationAction) => {
    sessionRef.current = dictationReducer(sessionRef.current, action);
    dispatch(action);
  }, []);

  useEffect(() => {
    sessionRef.current = session;
  }, [session]);
  useEffect(() => {
    modelsRef.current = models;
  }, [models]);
  useEffect(() => {
    void getPlatformCapabilities()
      .then(setPlatform)
      .catch(() => undefined);
  }, []);
  useEffect(() => {
    if (!isTauriRuntime() || platform?.platform !== "macos") return;
    // WKWebView can leave a new getUserMedia request pending after the app
    // becomes inactive. Acquire once while the initial window is foreground;
    // the disabled track can then be re-enabled by a global hotkey.
    void audioRef.current.prepare(setAudioDiagnostics).catch(() => undefined);
  }, [platform?.platform]);

  const endLocalResources = useCallback(async () => {
    startGenerationRef.current += 1;
    startControlSentRef.current = false;
    const acknowledgement = startAcknowledgementRef.current;
    startAcknowledgementRef.current = null;
    if (acknowledgement) {
      window.clearTimeout(acknowledgement.timer);
      acknowledgement.reject(new Error("Dictation start was cancelled"));
    }
    await audioRef.current.stop().catch(() => undefined);
    const queuedWrites = writerQueueRef.current;
    await queuedWrites.catch(() => undefined);
    if (writerQueueRef.current === queuedWrites) writerQueueRef.current = Promise.resolve();
    const lease = targetRef.current;
    targetRef.current = null;
    if (lease) await releaseTarget(lease.leaseId).catch(() => undefined);
    trackerRef.current = new TargetTracker();
    nativeRevisionRef.current = 0;
  }, []);

  const abortActiveSession = useCallback(() => {
    try {
      clientRef.current?.sendControl({ type: "stop" });
    } catch {
      clientRef.current?.close();
    }
    void endLocalResources();
  }, [endLocalResources]);

  const markTargetUnsafe = useCallback((message: string) => {
    setDeliveryPolicy("overlay_clipboard");
    setDeliveryReason(message);
  }, []);

  const abandonDirectTarget = useCallback(
    (message: string) => {
      const lease = targetRef.current;
      targetRef.current = null;
      markTargetUnsafe(message);
      if (lease) void releaseTarget(lease.leaseId).catch(() => undefined);
    },
    [markTargetUnsafe],
  );

  const queueStableInsert = useCallback(
    (segmentId: string, revision: number, stableText: string) => {
      const lease = targetRef.current;
      if (!lease || lease.policy !== "direct") return;
      const append = trackerRef.current.acceptStablePrefix(segmentId, revision, stableText);
      if (!append) return;
      writerQueueRef.current = writerQueueRef.current
        .then(async () => {
          nativeRevisionRef.current = await insertStableText(
            lease.leaseId,
            nativeRevisionRef.current,
            append.expectedPrefix,
            append.text,
          );
        })
        .catch((error: unknown) => {
          const message = `The typing target changed: ${friendlyError(error)}`;
          abandonDirectTarget(message);
        });
    },
    [abandonDirectTarget],
  );

  const queueCorrection = useCallback(
    (event: CorrectionPatch) => {
      const lease = targetRef.current;
      if (!lease || lease.policy !== "direct") return;
      const patch = trackerRef.current.planCorrection(event);
      if (!patch) return;
      writerQueueRef.current = writerQueueRef.current
        .then(async () => {
          nativeRevisionRef.current = await applyNativePatch({
            leaseId: lease.leaseId,
            baseRevision: nativeRevisionRef.current,
            ...patch,
          });
          trackerRef.current.commitCorrection(event);
        })
        .catch((error: unknown) => {
          const message = `Correction was not typed because the target changed: ${friendlyError(error)}`;
          abandonDirectTarget(message);
        });
    },
    [abandonDirectTarget],
  );

  const queueFinalInsert = useCallback(
    (segmentId: string, revision: number, finalText: string) => {
      const lease = targetRef.current;
      if (!lease || lease.policy !== "direct") return;
      const mutation = trackerRef.current.acceptFinal(segmentId, revision, finalText);
      if (!mutation) return;
      writerQueueRef.current = writerQueueRef.current
        .then(async () => {
          if (mutation.kind === "append") {
            nativeRevisionRef.current = await insertStableText(
              lease.leaseId,
              nativeRevisionRef.current,
              mutation.append.expectedPrefix,
              mutation.append.text,
            );
          } else {
            nativeRevisionRef.current = await applyNativePatch({
              leaseId: lease.leaseId,
              baseRevision: nativeRevisionRef.current,
              ...mutation.patch,
            });
          }
        })
        .catch((error: unknown) => {
          const message = `The final text target changed: ${friendlyError(error)}`;
          abandonDirectTarget(message);
        });
    },
    [abandonDirectTarget],
  );

  const handleEvent = useCallback(
    async (event: ServerEvent) => {
      switch (event.type) {
        case "session_started": {
          emit({ type: "started", sessionId: event.sessionId });
          const acknowledgement = startAcknowledgementRef.current;
          if (acknowledgement?.sessionId === event.sessionId) {
            startAcknowledgementRef.current = null;
            window.clearTimeout(acknowledgement.timer);
            acknowledgement.resolve();
          }
          break;
        }
        case "partial_transcript":
          emit({ type: "partial", event });
          queueStableInsert(event.segmentId, event.revision, event.stableText);
          break;
        case "segment_final":
          emit({ type: "final", event });
          queueFinalInsert(event.segmentId, event.revision, event.text);
          break;
        case "correction_patch":
          try {
            const segment = sessionRef.current.segments.find((item) => item.id === event.segmentId);
            if (!segment || segment.revision !== event.baseRevision) return;
            const actualHash = await sha256Hex(segment.text);
            if (actualHash.toLowerCase() !== event.rawTextSha256.toLowerCase()) return;
            emit({ type: "correction", event });
            queueCorrection(event);
          } catch (error) {
            emit({
              type: "fail",
              message: `Could not verify the server correction: ${friendlyError(error)}`,
            });
            abortActiveSession();
          }
          break;
        case "session_stopped":
          emit({ type: "stopped" });
          await endLocalResources();
          break;
        case "error":
          if (startAcknowledgementRef.current) {
            const acknowledgement = startAcknowledgementRef.current;
            startAcknowledgementRef.current = null;
            window.clearTimeout(acknowledgement.timer);
            acknowledgement.reject(new Error(event.message));
          }
          if (
            sessionRef.current.phase === "arming" ||
            sessionRef.current.phase === "listening" ||
            sessionRef.current.phase === "finalizing"
          ) {
            emit({ type: "fail", message: event.message });
            abortActiveSession();
          } else {
            setConnectionError(event.message);
          }
          break;
        case "ready":
        case "pong":
          break;
      }
    },
    [abortActiveSession, emit, endLocalResources, queueCorrection, queueFinalInsert, queueStableInsert],
  );

  const disconnect = useCallback(() => {
    clientRef.current?.close();
    clientRef.current = null;
    setConnection("disconnected");
    setCapabilities(null);
    setModels([]);
    automaticDownloadAttemptsRef.current.clear();
    automaticActivationAttemptsRef.current.clear();
    void endLocalResources();
    if (sessionRef.current.phase !== "idle")
      emit({ type: "fail", message: "The inference server disconnected" });
  }, [emit, endLocalResources]);

  const connect = useCallback(async () => {
    clientRef.current?.close();
    clientRef.current = null;
    automaticDownloadAttemptsRef.current.clear();
    automaticActivationAttemptsRef.current.clear();
    setConnection("connecting");
    setConnectionError(null);
    const client = new OpenFlowServerClient({
      baseUrl: settings.serverUrl,
      authToken: settings.authToken,
      onEvent: handleEvent,
      onDisconnect: (reason) => {
        if (clientRef.current !== client) return;
        setConnection("error");
        setConnectionError(reason);
        void endLocalResources();
        if (sessionRef.current.phase !== "idle") emit({ type: "fail", message: reason });
      },
    });
    try {
      const [nextCapabilities, nextModels] = await Promise.all([client.capabilities(), client.models()]);
      await client.connectStream();
      clientRef.current = client;
      setCapabilities(nextCapabilities);
      setModels(nextModels);
      setConnection("connected");
    } catch (error) {
      client.close();
      setConnection("error");
      setConnectionError(friendlyError(error, "Unable to connect to the inference server"));
      throw error;
    }
  }, [emit, endLocalResources, handleEvent, settings.authToken, settings.serverUrl]);

  const refreshModels = useCallback(async () => {
    if (!clientRef.current) return;
    setModels(await clientRef.current.models());
  }, []);

  useEffect(() => {
    if (connection !== "connected") return;
    if (
      !models.some(
        (model) =>
          model.state === "downloading" || model.state === "verifying" || model.state === "cancelling",
      )
    )
      return;
    const timer = window.setTimeout(() => {
      const client = clientRef.current;
      if (!client) return;
      void client
        .models()
        .then(setModels)
        .catch((error: unknown) =>
          setConnectionError(friendlyError(error, "Could not refresh model download progress")),
        );
    }, 1000);
    return () => window.clearTimeout(timer);
  }, [connection, models]);

  useEffect(() => {
    if (connection !== "connected") return;
    const client = clientRef.current;
    if (!client) return;
    const selectedIds = [settings.asrModelId, settings.cleanupModelId].filter(Boolean);
    const selectedModels = selectedIds
      .map((id) => models.find((model) => model.id === id))
      .filter((model): model is ModelSpec => Boolean(model));
    const memoryBudget = capabilities?.hardware.availableMemoryBytes ?? 0;
    const selectedMemory = selectedModels.reduce((total, model) => total + model.estimatedMemoryBytes, 0);
    const fits = memoryBudget === 0 || selectedMemory <= Math.floor(memoryBudget * 0.8);
    if (!fits) return;

    const actions: Array<() => Promise<void>> = [];
    for (const model of selectedModels) {
      if (model.state === "available" && !automaticDownloadAttemptsRef.current.has(model.id)) {
        automaticDownloadAttemptsRef.current.add(model.id);
        actions.push(() => client.downloadModel(model.id));
      }
      if (model.state === "cached" && !automaticActivationAttemptsRef.current.has(model.id)) {
        automaticActivationAttemptsRef.current.add(model.id);
        actions.push(() => client.activateModel(model.id));
      }
    }
    if (actions.length > 0) {
      // Activate the ASR model before cleanup so the server can prewarm the
      // complete pair instead of racing two independent activation requests.
      void (async () => {
        let failure: unknown;
        for (const action of actions) {
          try {
            await action();
          } catch (error) {
            failure ??= error;
          }
        }
        try {
          setModels(await client.models());
          if (failure) {
            setConnectionError(friendlyError(failure, "Could not prepare the selected models"));
          }
        } catch (error) {
          setConnectionError(friendlyError(error, "Could not refresh the selected models"));
        }
      })();
    }
  }, [
    capabilities?.hardware.availableMemoryBytes,
    connection,
    models,
    settings.asrModelId,
    settings.cleanupModelId,
  ]);

  const runModelAction = useCallback(async (action: (client: OpenFlowServerClient) => Promise<void>) => {
    const client = clientRef.current;
    if (!client) throw new Error("Connect to the inference server first");
    await action(client);
    setModels(await client.models());
  }, []);

  const start = useCallback(async () => {
    if (sessionRef.current.phase !== "idle" && sessionRef.current.phase !== "error") return;
    const client = clientRef.current;
    if (!client || connection !== "connected") throw new Error("Connect to the inference server first");
    const ready = (id: string) => {
      const model = modelsRef.current.find((candidate) => candidate.id === id);
      return model?.state === "cached" || model?.state === "active";
    };
    if (!settings.asrModelId || !ready(settings.asrModelId)) {
      throw new Error("Wait for the selected speech model to finish downloading");
    }
    if (settings.cleanupModelId && !ready(settings.cleanupModelId)) {
      throw new Error("Wait for the selected cleanup model to finish downloading");
    }
    const openFlowWindowFocused = await isOpenFlowWindowFocused();
    const sessionId = randomUuid();
    const startGeneration = startGenerationRef.current + 1;
    startGenerationRef.current = startGeneration;
    startControlSentRef.current = false;
    setDeliveryPolicy(null);
    setDeliveryReason(null);
    emit({ type: "arm", requestId: sessionId, now: Date.now() });
    let startSent = false;
    try {
      const lease = openFlowWindowFocused
        ? {
            leaseId: 0,
            policy: "overlay_clipboard" as const,
            initialRevision: 0,
            reason:
              "OpenFlow itself is focused, so direct insertion is disabled. Focus another application and use the hotkey to type there.",
          }
        : await captureTarget();
      if (startGenerationRef.current !== startGeneration) {
        await releaseTarget(lease.leaseId).catch(() => undefined);
        emit({ type: "stopped" });
        return;
      }
      if (lease.policy === "blocked") throw new Error(lease.reason);
      targetRef.current = lease;
      setDeliveryPolicy(lease.policy);
      setDeliveryReason(lease.reason);
      nativeRevisionRef.current = lease.initialRevision;
      trackerRef.current = new TargetTracker();
      const started = new Promise<void>((resolve, reject) => {
        const timer = window.setTimeout(
          () => {
            if (startAcknowledgementRef.current?.sessionId !== sessionId) return;
            startAcknowledgementRef.current = null;
            reject(new Error("The inference server did not prepare the selected models in time"));
          },
          6 * 60 * 1000,
        );
        startAcknowledgementRef.current = { sessionId, timer, resolve, reject };
      });
      client.sendControl({
        type: "start",
        payload: {
          sessionId,
          asrModelId: settings.asrModelId || null,
          cleanupModelId: settings.cleanupModelId || null,
          language: settings.language === "auto" ? null : settings.language,
          sampleRateHz: 16000,
          channels: 1,
          audioEncoding: "pcm_s16_le",
          glossary: [],
          options: {},
        },
      });
      startSent = true;
      startControlSentRef.current = true;
      await started;
      if (startGenerationRef.current !== startGeneration) {
        await endLocalResources();
        emit({ type: "stopped" });
        return;
      }
      boundaryRef.current = new SpeechBoundaryDetector();
      await audioRef.current.start((frame) => {
        if (!client.sendAudio(frame)) {
          emit({
            type: "fail",
            message: "Audio stopped because the server or network is not keeping up",
          });
          abortActiveSession();
          return;
        }
        if (boundaryRef.current.process(frame)) client.sendControl({ type: "commit" });
      }, setAudioDiagnostics);
      if (startGenerationRef.current !== startGeneration) {
        await audioRef.current.stop().catch(() => undefined);
      }
    } catch (error) {
      if (startGenerationRef.current !== startGeneration) {
        await endLocalResources();
        emit({ type: "stopped" });
        return;
      }
      if (startSent) {
        try {
          client.sendControl({ type: "stop" });
        } catch {
          client.close();
        }
      }
      if (startAcknowledgementRef.current?.sessionId === sessionId) {
        const acknowledgement = startAcknowledgementRef.current;
        startAcknowledgementRef.current = null;
        window.clearTimeout(acknowledgement.timer);
      }
      await endLocalResources();
      emit({ type: "fail", message: friendlyError(error, "Could not start dictation") });
      throw error;
    }
  }, [
    abortActiveSession,
    connection,
    emit,
    endLocalResources,
    settings.asrModelId,
    settings.cleanupModelId,
    settings.language,
  ]);

  const stop = useCallback(async () => {
    const current = sessionRef.current;
    if (current.phase !== "listening" && current.phase !== "arming") return;
    emit({ type: "stop" });
    startGenerationRef.current += 1;
    const acknowledgement = startAcknowledgementRef.current;
    startAcknowledgementRef.current = null;
    if (acknowledgement) {
      window.clearTimeout(acknowledgement.timer);
      acknowledgement.reject(new Error("Dictation start was cancelled"));
    }
    await audioRef.current.stop().catch(() => undefined);
    if (startControlSentRef.current) clientRef.current?.sendControl({ type: "stop" });
  }, [emit]);

  const toggle = useCallback(async () => {
    const phase = sessionRef.current.phase;
    if (phase === "listening" || phase === "arming") await stop();
    else if (phase === "idle" || phase === "error") await start();
  }, [start, stop]);

  useEffect(
    () => () => {
      clientRef.current?.close();
      void endLocalResources().finally(() => audioRef.current.dispose());
    },
    [endLocalResources],
  );

  const downloadModel = useCallback(
    async (id: string) => {
      // A manual selection is the user's explicit consent to download this
      // model. Record it before settings propagation so the preparation effect
      // cannot submit a duplicate request for the same selection.
      automaticDownloadAttemptsRef.current.add(id);
      try {
        await runModelAction((client) => client.downloadModel(id));
      } catch (error) {
        automaticDownloadAttemptsRef.current.delete(id);
        throw error;
      }
    },
    [runModelAction],
  );
  const activateModel = useCallback(
    async (id: string) => {
      automaticActivationAttemptsRef.current.add(id);
      try {
        await runModelAction((client) => client.activateModel(id));
      } catch (error) {
        automaticActivationAttemptsRef.current.delete(id);
        throw error;
      }
    },
    [runModelAction],
  );
  const cancelModelDownload = useCallback(
    (id: string) => {
      // Cancellation is a deliberate pause. Keep this model in the attempted
      // set so the selection-driven preparation effect does not immediately
      // restart it when the server returns to `not_downloaded`.
      automaticDownloadAttemptsRef.current.add(id);
      return runModelAction((client) => client.cancelModelDownload(id));
    },
    [runModelAction],
  );
  const deactivateModel = useCallback(
    (id: string) => runModelAction((client) => client.deactivateModel(id)),
    [runModelAction],
  );
  const deleteModel = useCallback(
    (id: string) => runModelAction((client) => client.deleteModel(id)),
    [runModelAction],
  );
  const pair = useCallback(
    async (pairingCode: string, deviceName: string) => {
      const client = new OpenFlowServerClient({
        baseUrl: settings.serverUrl,
        onEvent: () => undefined,
        onDisconnect: () => undefined,
      });
      return (await client.pair(pairingCode, deviceName)).deviceToken;
    },
    [settings.serverUrl],
  );
  const pairInteractively = useCallback(
    async (deviceName: string, verificationCode: string) => {
      const client = new OpenFlowServerClient({
        baseUrl: settings.serverUrl,
        onEvent: () => undefined,
        onDisconnect: () => undefined,
      });
      return (await client.pairInteractively(deviceName, verificationCode)).deviceToken;
    },
    [settings.serverUrl],
  );

  return useMemo(
    () => ({
      connection,
      connectionError,
      capabilities,
      platform,
      deliveryPolicy,
      deliveryReason,
      models,
      session,
      transcript: renderedTranscript(session),
      audioDiagnostics,
      connect,
      disconnect,
      toggle,
      start,
      stop,
      refreshModels,
      downloadModel,
      cancelModelDownload,
      activateModel,
      deactivateModel,
      deleteModel,
      pair,
      pairInteractively,
    }),
    [
      activateModel,
      audioDiagnostics,
      capabilities,
      cancelModelDownload,
      connect,
      connection,
      connectionError,
      deactivateModel,
      deleteModel,
      disconnect,
      downloadModel,
      deliveryPolicy,
      deliveryReason,
      models,
      pair,
      pairInteractively,
      platform,
      refreshModels,
      runModelAction,
      session,
      start,
      stop,
      toggle,
    ],
  );
}
