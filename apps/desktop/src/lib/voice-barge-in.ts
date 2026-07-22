// VAD barge-in: watch the mic while TTS plays and fire when the user starts
// talking over it. Echo cancellation strips the app's own speaker output from
// the capture, the noise floor is calibrated while playback is already
// audible, and the sustained window filters coughs/thumps — mirrors
// tools/voice_mode.listen_for_speech on the Python surfaces.

const CALIBRATION_MS = 400
const SUSTAINED_MS = 300
const MIN_TRIGGER_LEVEL = 0.075 // matches the voice loop's silenceLevel

export function monitorSpeechDuringPlayback(onSpeech: () => void): () => void {
  let disposed = false
  let stream: MediaStream | null = null
  let context: AudioContext | null = null
  let frame: number | null = null

  const cleanup = () => {
    disposed = true

    if (frame !== null) {
      window.cancelAnimationFrame(frame)
      frame = null
    }

    void context?.close().catch(() => undefined)
    context = null
    stream?.getTracks().forEach(track => track.stop())
    stream = null
  }
  void (async () => {
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true }
      })

      if (disposed) {
        cleanup()

        return
      }

      context = new AudioContext()
      const analyser = context.createAnalyser()
      analyser.fftSize = 256
      context.createMediaStreamSource(stream).connect(analyser)

      const data = new Uint8Array(analyser.fftSize)
      const startedAt = Date.now()
      const floorSamples: number[] = []
      let speechStartedAt: number | null = null

      const tick = () => {
        if (disposed) {
          return
        }

        analyser.getByteTimeDomainData(data)

        let sum = 0

        for (const value of data) {
          const centered = value - 128
          sum += centered * centered
        }

        const level = Math.min(1, Math.sqrt(sum / data.length) / 42)
        const now = Date.now()

        if (now - startedAt < CALIBRATION_MS) {
          floorSamples.push(level)
        } else {
          const floor = floorSamples.length ? [...floorSamples].sort((a, b) => a - b)[floorSamples.length >> 1] : 0
          const trigger = Math.max(MIN_TRIGGER_LEVEL, floor * 3.5)

          if (level >= trigger) {
            speechStartedAt ??= now

            if (now - speechStartedAt >= SUSTAINED_MS) {
              cleanup()
              onSpeech()

              return
            }
          } else {
            speechStartedAt = null
          }
        }

        frame = window.requestAnimationFrame(tick)
      }

      tick()
    } catch {
      cleanup()
    }
  })()

  return cleanup
}
