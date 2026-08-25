class OpenFlowPcmCapture extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.targetRate = 16000;
    const configuredInputRate = options?.processorOptions?.inputSampleRate;
    this.inputRate =
      Number.isFinite(configuredInputRate) && configuredInputRate > 0 ? configuredInputRate : sampleRate;
    this.ratio = this.inputRate / this.targetRate;
    // Fewer, larger WebSocket messages keep capture independent of inference
    // latency while retaining 100 ms resolution for the client-side VAD.
    this.frameSamples = 1600;
    this.readPosition = 0;
    this.source = [];
    this.output = [];
    this.port.onmessage = (event) => {
      if (event.data?.type !== "flush") return;
      // Duplicate the final sample so linear interpolation can consume the
      // otherwise stranded tail, then emit a short final PCM frame.
      if (this.source.length > 0) this.source.push(this.source[this.source.length - 1]);
      this.resample();
      this.emitOutput();
      this.source = [];
      this.readPosition = 0;
      this.port.postMessage({ type: "flushed" });
    };
  }

  resample() {
    while (this.readPosition + 1 < this.source.length) {
      const left = Math.floor(this.readPosition);
      const mix = this.readPosition - left;
      this.output.push(this.source[left] * (1 - mix) + this.source[left + 1] * mix);
      this.readPosition += this.ratio;

      if (this.output.length === this.frameSamples) {
        this.emitOutput();
      }
    }

    // Keep the last source sample for interpolation with the next render
    // quantum. `readPosition` can advance beyond the current source array.
    const consumed = Math.min(Math.floor(this.readPosition), Math.max(0, this.source.length - 1));
    if (consumed > 0) {
      this.source.splice(0, consumed);
      this.readPosition -= consumed;
    }
  }

  emitOutput() {
    if (this.output.length === 0) return;
    const frame = new Int16Array(this.output.length);
    for (let index = 0; index < frame.length; index += 1) {
      const value = Math.max(-1, Math.min(1, this.output[index]));
      frame[index] = value < 0 ? value * 0x8000 : value * 0x7fff;
    }
    this.port.postMessage(frame.buffer, [frame.buffer]);
    this.output = [];
  }

  process(inputs) {
    const channel = inputs[0]?.[0];
    if (!channel) return true;
    for (let index = 0; index < channel.length; index += 1) this.source.push(channel[index]);
    this.resample();
    return true;
  }
}

registerProcessor("openflow-pcm-capture", OpenFlowPcmCapture);
