// pi3-emu PWM audio worklet: the main thread posts Float32Array sample
// batches (guest square-wave notes, -1..1 floats) and the worklet queues
// them to the speakers at the context rate. Replaces the deprecated
// ScriptProcessor path (see initAudio in src/main.js); same 0.3 gain and
// silence-when-empty semantics.
class Pi3PwmPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.queue = [];
    this.head = 0;
    this.port.onmessage = (e) => {
      const data = e.data;
      if (!data) return;
      for (let i = 0; i < data.length; i++) this.queue.push(data[i]);
    };
  }
  process(inputs, outputs) {
    const out = outputs[0][0];
    for (let i = 0; i < out.length; i++) {
      out[i] = this.head < this.queue.length ? this.queue[this.head++] * 0.3 : 0;
    }
    // Compact the consumed prefix so the queue never grows without bound.
    if (this.head > 65536) {
      this.queue = this.queue.slice(this.head);
      this.head = 0;
    }
    return true;
  }
}
registerProcessor('pi3-pwm', Pi3PwmPlayer);
