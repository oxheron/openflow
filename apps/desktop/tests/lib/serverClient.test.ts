import { describe, expect, it } from "vitest";
import {
  OrderedEventDispatcher,
  assertProtocolVersion,
  canQueueAudio,
  decodeServerFrame,
  normalizedBaseUrl,
} from "../../src/lib/serverClient";

describe("server URL validation", () => {
  it("allows TLS endpoints and explicit loopback development endpoints", () => {
    expect(normalizedBaseUrl("https://openflow.example/base/").toString()).toBe(
      "https://openflow.example/base",
    );
    expect(normalizedBaseUrl("http://127.0.0.1:8765/").toString()).toBe("http://127.0.0.1:8765/");
    expect(normalizedBaseUrl("http://localhost:8765").toString()).toBe("http://localhost:8765/");
  });

  it("rejects plaintext remote servers before pairing or authentication", () => {
    expect(() => normalizedBaseUrl("http://192.0.2.10:8765")).toThrow(
      "Remote OpenFlow servers require HTTPS",
    );
  });

  it("rejects credentials and ambiguous URL suffixes", () => {
    expect(() => normalizedBaseUrl("https://user:secret@openflow.example")).toThrow("embedded credentials");
    expect(() => normalizedBaseUrl("https://openflow.example?token=secret")).toThrow("query or fragment");
    expect(() => normalizedBaseUrl("wss://openflow.example")).toThrow("http:// or https://");
  });
});

describe("stream safety", () => {
  it("preserves wire order across asynchronous event verification", async () => {
    const observed: string[] = [];
    let releaseCorrection: (() => void) | undefined;
    const correction = new Promise<void>((resolve) => {
      releaseCorrection = resolve;
    });
    const dispatcher = new OrderedEventDispatcher<string>(
      async (event) => {
        if (event === "correction") await correction;
        observed.push(event);
      },
      () => undefined,
    );

    dispatcher.dispatch("final");
    dispatcher.dispatch("correction");
    dispatcher.dispatch("stopped");
    await Promise.resolve();
    await Promise.resolve();
    expect(observed).toEqual(["final"]);
    releaseCorrection?.();
    await correction;
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(observed).toEqual(["final", "correction", "stopped"]);
  });

  it("bounds browser-side websocket audio buffering", () => {
    expect(canQueueAudio(900, 100, 1000)).toBe(true);
    expect(canQueueAudio(901, 100, 1000)).toBe(false);
    expect(canQueueAudio(-1, 100, 1000)).toBe(false);
  });

  it("fails closed on incompatible versions and malformed server frames", () => {
    expect(() => assertProtocolVersion(2)).toThrow("expected 1");
    expect(() => assertProtocolVersion(1.5)).toThrow("expected 1");
    expect(() => decodeServerFrame(new ArrayBuffer(1))).toThrow("non-text");
    expect(() => decodeServerFrame("not json")).toThrow("malformed JSON");
    expect(() => decodeServerFrame('{"type":"partial","payload":{}}')).toThrow("invalid OpenFlow event");
  });
});
