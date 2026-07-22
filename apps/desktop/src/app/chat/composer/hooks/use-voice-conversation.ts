import { useCallback, useEffect, useRef, useState } from 'react'

import { useI18n } from '@/i18n'
import { monitorSpeechDuringPlayback } from '@/lib/voice-barge-in'
import {
  playSpeechText,
  type SpeechStreamSession,
  startSpeechStream,
  stopVoicePlayback
} from '@/lib/voice-playback'
import { notify, notifyError } from '@/store/notifications'

import { useMicRecorder } from './use-mic-recorder'

export type ConversationStatus = 'idle' | 'listening' | 'transcribing' | 'thinking' | 'speaking'

interface PendingVoiceResponse {
  id: string
  pending: boolean
  text: string
}

interface VoiceConversationOptions {
  busy: boolean
  enabled: boolean
  onFatalError?: () => void
  onSubmit: (text: string) => Promise<void> | void
  onTranscribeAudio?: (audio: Blob) => Promise<string>
  pendingResponse: () => PendingVoiceResponse | null
  consumePendingResponse: () => void
}

export function useVoiceConversation({
  busy,
  enabled,
  onFatalError,
  onSubmit,
  onTranscribeAudio,
  pendingResponse,
  consumePendingResponse
}: VoiceConversationOptions) {
  const { t } = useI18n()
  const voiceCopy = t.notifications.voice
  const { handle, level } = useMicRecorder(voiceCopy)
  const [status, setStatus] = useState<ConversationStatus>('idle')
  const [muted, setMuted] = useState(false)
  const turnTimeoutRef = useRef<number | null>(null)
  const pendingStartRef = useRef(false)
  const turnClosingRef = useRef(false)
  const awaitingSpokenResponseRef = useRef(false)
  const responseIdRef = useRef<string | null>(null)
  const spokenSourceLengthRef = useRef(0)
  const speechSessionRef = useRef<null | SpeechStreamSession>(null)
  const stopBargeMonitorRef = useRef<(() => void) | null>(null)
  const enabledRef = useRef(enabled)
  const mutedRef = useRef(muted)
  const busyRef = useRef(busy)
  const statusRef = useRef<ConversationStatus>('idle')
  const wasEnabledRef = useRef(enabled)

  useEffect(() => {
    enabledRef.current = enabled
  }, [enabled])

  useEffect(() => {
    mutedRef.current = muted
  }, [muted])

  useEffect(() => {
    busyRef.current = busy
  }, [busy])

  useEffect(() => {
    statusRef.current = status
  }, [status])

  const clearTurnTimeout = () => {
    if (turnTimeoutRef.current) {
      window.clearTimeout(turnTimeoutRef.current)
      turnTimeoutRef.current = null
    }
  }

  const dropSpeechSession = () => {
    stopBargeMonitorRef.current?.()
    stopBargeMonitorRef.current = null
    speechSessionRef.current = null
    responseIdRef.current = null
    spokenSourceLengthRef.current = 0
  }

  const handleTurn = useCallback(
    async (forceTranscribe = false) => {
      if (turnClosingRef.current) {
        return
      }

      turnClosingRef.current = true
      clearTurnTimeout()
      setStatus('transcribing')

      try {
        const result = await handle.stop()

        if (!result || (!result.heardSpeech && !forceTranscribe) || !onTranscribeAudio) {
          if (enabledRef.current && !mutedRef.current && !busyRef.current && statusRef.current !== 'speaking') {
            pendingStartRef.current = true
          }

          setStatus('idle')

          return
        }

        try {
          const transcript = (await onTranscribeAudio(result.audio)).trim()

          if (!transcript) {
            if (enabledRef.current) {
              pendingStartRef.current = true
            }

            setStatus('idle')

            return
          }

          awaitingSpokenResponseRef.current = true
          dropSpeechSession()
          await onSubmit(transcript)
          setStatus('thinking')
        } catch (error) {
          notifyError(error, voiceCopy.transcriptionFailed)

          if (enabledRef.current && !mutedRef.current && !busyRef.current) {
            pendingStartRef.current = true
          }

          setStatus('idle')
        }
      } finally {
        turnClosingRef.current = false
      }
    },
    [handle, onSubmit, onTranscribeAudio, voiceCopy.transcriptionFailed]
  )

  const startListening = useCallback(async () => {
    pendingStartRef.current = false

    if (!enabledRef.current || mutedRef.current || busyRef.current) {
      return
    }

    if (statusRef.current !== 'idle') {
      return
    }

    try {
      // VAD tuning mirrors `tools.voice_mode` defaults so the browser loop matches the CLI.
      await handle.start({
        silenceLevel: 0.075,
        silenceMs: 1_250,
        idleSilenceMs: 12_000,
        onError: error => {
          notifyError(error, voiceCopy.microphoneFailed)
          pendingStartRef.current = false
          onFatalError?.()
        },
        onSilence: () => void handleTurn()
      })
      setStatus('listening')
      turnTimeoutRef.current = window.setTimeout(() => void handleTurn(), 60_000)
    } catch (error) {
      notifyError(error, voiceCopy.couldNotStartSession)
      pendingStartRef.current = false
      setStatus('idle')
      onFatalError?.()
    }
  }, [handle, handleTurn, onFatalError, voiceCopy.couldNotStartSession, voiceCopy.microphoneFailed])

  const settleAfterSpeech = useCallback(
    (barged: boolean) => {
      dropSpeechSession()

      if (barged || !awaitingSpokenResponseRef.current) {
        awaitingSpokenResponseRef.current = false
        consumePendingResponse()
      }

      if (enabledRef.current) {
        pendingStartRef.current = true
      }

      setStatus('idle')
    },
    [consumePendingResponse]
  )

  /** Push any new reply text into the live session; finish when complete. */
  const feedSpeechSession = useCallback(
    (responseId: string) => {
      const session = speechSessionRef.current

      if (!session || responseIdRef.current !== responseId) {
        return
      }

      const response = pendingResponse()

      if (response && response.id === responseId) {
        if (response.text.length > spokenSourceLengthRef.current) {
          session.append(response.text.slice(spokenSourceLengthRef.current))
          spokenSourceLengthRef.current = response.text.length
        }

        if (!response.pending && !busyRef.current) {
          session.finish()
        }
      } else if (!busyRef.current) {
        // Reply consumed/vanished while we were speaking — close out the turn.
        session.finish()
      }
    },
    [pendingResponse]
  )

  /** Whole-text fallback: wait for the reply to complete, then speak it. */
  const awaitFallbackSpeech = useCallback(
    (responseId: string) => {
      const poll = () => {
        if (responseIdRef.current !== responseId) {
          return
        }

        const response = pendingResponse()

        if (!response || response.id !== responseId) {
          settleAfterSpeech(false)

          return
        }

        if (response.pending || busyRef.current) {
          window.setTimeout(poll, 250)

          return
        }

        let barged = false

        stopBargeMonitorRef.current?.()
        stopBargeMonitorRef.current = monitorSpeechDuringPlayback(() => {
          barged = true
          stopVoicePlayback()
        })

        void playSpeechText(response.text, { source: 'voice-conversation' })
          .catch(error => notifyError(error, voiceCopy.playbackFailed))
          .finally(() => {
            if (responseIdRef.current === responseId) {
              awaitingSpokenResponseRef.current = false
              settleAfterSpeech(barged)
            }
          })
      }

      poll()
    },
    [pendingResponse, settleAfterSpeech, voiceCopy.playbackFailed]
  )

  /**
   * Live-speak the streaming reply: one speech session per response, fed
   * incremental text as the assistant generates it. Audio overlaps generation
   * — no wait for the full reply, no per-sentence gaps.
   */
  const openLiveSpeech = useCallback(
    (responseId: string) => {
      responseIdRef.current = responseId
      spokenSourceLengthRef.current = 0
      setStatus('speaking')

      let barged = false

      // VAD barge-in: the user talking over the reply cuts playback AND drops
      // the not-yet-spoken remainder, so the loop goes straight back to
      // listening instead of finishing the interrupted answer.
      stopBargeMonitorRef.current = monitorSpeechDuringPlayback(() => {
        barged = true
        stopVoicePlayback()
      })

      void (async () => {
        const session = await startSpeechStream({ source: 'voice-conversation' })

        // The session may resolve after the loop moved on (barge, disable).
        if (responseIdRef.current !== responseId) {
          if (session) {
            stopVoicePlayback()
          }

          return
        }

        if (!session) {
          // No streaming backend/provider: speak the whole reply once it lands.
          speechSessionRef.current = null
          awaitFallbackSpeech(responseId)

          return
        }

        speechSessionRef.current = session

        // Timer-driven feed: reply text flows into the session at delta rate
        // regardless of React render cadence.
        const feedTimer = window.setInterval(() => feedSpeechSession(responseId), 150)
        feedSpeechSession(responseId)

        const outcome = await session.done
        window.clearInterval(feedTimer)

        if (responseIdRef.current !== responseId) {
          return
        }

        if (outcome === 'fallback') {
          awaitFallbackSpeech(responseId)

          return
        }

        awaitingSpokenResponseRef.current = false
        settleAfterSpeech(barged)
      })()
    },
    [awaitFallbackSpeech, feedSpeechSession, settleAfterSpeech]
  )

  const start = useCallback(async () => {
    if (!onTranscribeAudio) {
      notify({
        kind: 'warning',
        title: voiceCopy.unavailable,
        message: voiceCopy.configureSpeechToText
      })
      onFatalError?.()

      return
    }

    setMuted(false)
    awaitingSpokenResponseRef.current = false
    dropSpeechSession()
    consumePendingResponse()
    pendingStartRef.current = true
    await startListening()
  }, [
    consumePendingResponse,
    onFatalError,
    onTranscribeAudio,
    startListening,
    voiceCopy.configureSpeechToText,
    voiceCopy.unavailable
  ])

  const end = useCallback(async () => {
    pendingStartRef.current = false
    clearTurnTimeout()
    stopVoicePlayback()
    handle.cancel()
    turnClosingRef.current = false
    awaitingSpokenResponseRef.current = false
    dropSpeechSession()
    consumePendingResponse()
    setMuted(false)
    setStatus('idle')
  }, [consumePendingResponse, handle])

  const stopTurn = useCallback(() => {
    if (statusRef.current === 'listening') {
      void handleTurn(true)
    }
  }, [handleTurn])

  const toggleMute = useCallback(() => {
    setMuted(value => {
      const next = !value

      if (next) {
        clearTurnTimeout()
        handle.cancel()
        setStatus('idle')
      } else if (enabledRef.current && !busyRef.current && statusRef.current === 'idle') {
        pendingStartRef.current = true
      }

      return next
    })
  }, [handle])

  useEffect(() => {
    if (!enabled) {
      return
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.code !== 'Space' || event.repeat || event.metaKey || event.ctrlKey || event.altKey) {
        return
      }

      if (statusRef.current !== 'listening') {
        return
      }

      event.preventDefault()
      stopTurn()
    }

    window.addEventListener('keydown', onKeyDown, { capture: true })

    return () => window.removeEventListener('keydown', onKeyDown, { capture: true })
  }, [enabled, stopTurn])

  // Drive the loop: when a voice-submitted reply appears, open a live speech
  // session (which feeds itself from then on). Otherwise start listening when
  // idle between turns.
  useEffect(() => {
    if (!enabled || muted) {
      return
    }

    if (awaitingSpokenResponseRef.current && status !== 'speaking') {
      const response = pendingResponse()

      if (response) {
        openLiveSpeech(response.id)

        return
      }

      if (!busy && status === 'thinking') {
        // Turn finished without any speakable reply (tool-only, error).
        awaitingSpokenResponseRef.current = false
        dropSpeechSession()
        pendingStartRef.current = true
        setStatus('idle')

        return
      }
    }

    if (busy || status !== 'idle') {
      return
    }

    if (pendingStartRef.current) {
      void startListening()
    }
  }, [busy, enabled, muted, openLiveSpeech, pendingResponse, startListening, status])

  useEffect(() => {
    if (enabled && !wasEnabledRef.current) {
      void start()
    }

    if (!enabled && wasEnabledRef.current) {
      void end()
    }

    wasEnabledRef.current = enabled
  }, [enabled, end, start])

  return { end, level, muted, start, status, stopTurn, toggleMute }
}
