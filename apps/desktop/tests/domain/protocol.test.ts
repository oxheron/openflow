import { describe, expect, it } from "vitest";
import {
  decodeCapabilities,
  decodeModels,
  decodeServerEvent,
  stableUtf8Prefix,
} from "../../src/domain/protocol";

describe("protocol decoding", () => {
  it("decodes flattened model download state", () => {
    const models = decodeModels([
      {
        id: "qwen-test",
        display_name: "Qwen test",
        kind: "text_cleanup",
        family: "llama.cpp",
        license: "Apache-2.0",
        size_bytes: 100,
        estimated_memory_bytes: 200,
        quantization: "Q4_K_M",
        metadata: { parameters: "4B", recommended: "true" },
        state: "downloading",
        downloaded_bytes: 25,
        total_bytes: 100,
        active: false,
      },
    ]);
    expect(models[0]).toMatchObject({
      license: "Apache-2.0",
      state: "downloading",
      progress: 0.25,
      recommended: true,
    });
  });

  it("selects a server-reported ROCm accelerator", () => {
    const capabilities = decodeCapabilities({
      protocol_version: 1,
      server_version: "test",
      hardware: {
        os: "linux",
        architecture: "x86_64",
        logical_cpus: 16,
        system_memory_bytes: 32 * 1024 ** 3,
        accelerator_memory_bytes: 8 * 1024 ** 3,
        backends: ["cpu", "rocm"],
      },
      max_audio_bytes_per_session: 2 * 1024 ** 2,
      active_session: false,
    });
    expect(capabilities.hardware.backend).toBe("rocm");
    expect(capabilities.hardware.deviceName).toBe("ROCM accelerator");
  });

  it("uses UTF-8 byte offsets without splitting characters", () => {
    expect(stableUtf8Prefix("héllo", 3)).toBe("hé");
    expect(stableUtf8Prefix("héllo", 2)).toBeNull();
  });

  it("decodes the Rust tagged partial envelope", () => {
    expect(
      decodeServerEvent({
        type: "partial",
        payload: {
          session_id: "session",
          segment_id: 2,
          revision: 3,
          sequence: 4,
          text: "hello",
          stable_prefix_bytes: 5,
          tokens: [],
        },
      }),
    ).toMatchObject({ type: "partial_transcript", segmentId: "2", stableText: "hello" });
  });
});
