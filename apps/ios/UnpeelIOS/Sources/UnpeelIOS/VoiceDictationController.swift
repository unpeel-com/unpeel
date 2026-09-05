//
//  VoiceDictationController.swift
//  UnpeelIOS
//
//  Push-to-talk dictation for the terminal. The system keyboard's inline
//  dictation cannot work against a terminal: it refines speech via ranged
//  replacements on the text document, and a byte-stream terminal has no
//  document to edit — only the first hypothesis fragment ever landed. This
//  controller owns the microphone + speech recognition directly, shows the
//  live transcript, and commits the FINAL text once, through the same
//  bracketed-paste path as typing.
//
//  Two recognition backends behind one `toggle(onFinish:)` API:
//  - iOS 26+: `SpeechAnalyzer` + `DictationTranscriber` (the on-device engine
//    that powers system dictation) — fully on-device, streaming volatile/final
//    results, better punctuation. `DictationTranscriber` reuses the installed
//    keyboard dictation models, so no large per-locale asset download gates the
//    first tap. See `ModernDictationBackend`.
//  - iOS 17–25 (or if the modern path can't start): `SFSpeechRecognizer`.
//
//  On iOS 26+ with Apple Intelligence available, the finished transcript can
//  additionally take a "reflection" pass through the on-device model before
//  commit (`DictationReflection.swift`): punctuation/filler cleanup, verbatim
//  fallback on any failure, skipped for short utterances like menu answers.
//

import Foundation
#if os(iOS)
import AVFoundation
import Speech
#endif

@MainActor
final class VoiceDictationController: ObservableObject {
    @Published private(set) var isRecording = false
    /// The stopped transcript is being cleaned up by the on-device model
    /// before commit; the pill stays up showing progress.
    @Published private(set) var isRefining = false
    @Published private(set) var transcript = ""
    @Published private(set) var errorMessage: String?

    #if os(iOS)
    private let engine = AVAudioEngine()
    // Legacy (SFSpeechRecognizer) backend.
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    // Modern (SpeechAnalyzer) backend — held type-erased so stored properties
    // don't need an availability annotation the class can't carry.
    private var modernBackend: AnyObject?
    // Apple Intelligence reflection pass (same type-erasure trick).
    private var reflector: AnyObject?
    private var refineTask: Task<Void, Never>?
    #endif

    /// Tap to start, tap again to stop-and-commit. The transcript at stop
    /// time is delivered to `onFinish` — possibly after the reflection pass.
    func toggle(onFinish: @escaping (String) -> Void) {
        if isRefining {
            // A commit is already in flight; don't start a second one (the
            // pill's X cancels it).
            return
        }
        if isRecording {
            let text = transcript
            stopRecording()
            if !text.isEmpty {
                commit(text, onFinish: onFinish)
            }
        } else {
            start()
        }
    }

    func cancel() {
        #if os(iOS)
        refineTask?.cancel()
        refineTask = nil
        reflector = nil
        #endif
        isRefining = false
        stopRecording()
    }

    /// Commit a finished transcript: run the reflection pass when it applies,
    /// otherwise (or on any failure) deliver the verbatim text.
    private func commit(_ text: String, onFinish: @escaping (String) -> Void) {
        #if os(iOS) && canImport(FoundationModels)
        defer { reflector = nil }
        // Menu answers and confirmations ("yes", "2", "continue") skip the
        // pass — latency there is pure cost.
        let wordCount = text.split(whereSeparator: \.isWhitespace).count
        if #available(iOS 26.0, *),
           DictationSettings.shared.reflectionEnabled,
           wordCount >= 5,
           let reflector = reflector as? DictationReflecting {
            isRefining = true
            refineTask = Task { [weak self] in
                let refined = await reflector.refine(text)
                guard let self, !Task.isCancelled else { return }
                self.isRefining = false
                self.refineTask = nil
                onFinish(refined ?? text)
            }
            return
        }
        #endif
        onFinish(text)
    }

    private func start() {
        #if os(iOS)
        errorMessage = nil
        transcript = ""
        // Completion handlers below fire on TCC/speech background queues.
        // They MUST be explicitly @Sendable: a closure formed in this
        // @MainActor context otherwise inherits main-actor isolation, and
        // Swift 6's runtime isolation check traps (SIGTRAP in
        // dispatch_assert_queue) the moment the system invokes it off-main.
        SFSpeechRecognizer.requestAuthorization { @Sendable [weak self] status in
            Task { @MainActor in
                guard let self else { return }
                guard status == .authorized else {
                    self.errorMessage = "Speech recognition not allowed"
                    return
                }
                AVAudioApplication.requestRecordPermission { @Sendable granted in
                    Task { @MainActor in
                        guard granted else {
                            self.errorMessage = "Microphone not allowed"
                            return
                        }
                        self.beginRecognition()
                    }
                }
            }
        }
        #endif
    }

    #if os(iOS)
    private func beginRecognition() {
        // Warm the reflection model while the user talks, so the polish pass
        // adds minimal latency at stop time. No-op when Apple Intelligence is
        // unavailable — `commit` then falls through to verbatim.
        #if canImport(FoundationModels)
        if #available(iOS 26.0, *), DictationSettings.shared.reflectionEnabled {
            let reflector = DictationReflector()
            reflector.prepare()
            self.reflector = reflector
        }
        #endif
        // Prefer the modern on-device engine when available; fall back to
        // SFSpeechRecognizer if it can't start (unsupported locale, etc.).
        if #available(iOS 26.0, *) {
            beginModernRecognition()
        } else {
            beginLegacyRecognition()
        }
    }

    @available(iOS 26.0, *)
    private func beginModernRecognition() {
        let backend = ModernDictationBackend(engine: engine)
        modernBackend = backend
        backend.onTranscript = { [weak self] text in
            Task { @MainActor in self?.transcript = text }
        }
        backend.onFailure = { [weak self] message, recoverable in
            Task { @MainActor in
                guard let self else { return }
                // Setup failure before we were live → drop the modern backend
                // and retry on the classic recognizer so the user still gets
                // dictation. A mid-session failure just surfaces a note.
                if !self.isRecording {
                    self.modernBackend = nil
                    if recoverable { self.beginLegacyRecognition() }
                    else { self.errorMessage = message }
                } else {
                    self.errorMessage = message
                }
            }
        }
        Task { @MainActor in
            let started = await backend.start()
            if started {
                self.isRecording = true
            }
            // If not started, `onFailure` already routed to the legacy path.
        }
    }

    private func beginLegacyRecognition() {
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable else {
            errorMessage = "Speech recognition unavailable"
            return
        }
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            guard session.isInputAvailable else {
                errorMessage = "Microphone unavailable"
                stopRecording()
                return
            }

            let request = SFSpeechAudioBufferRecognitionRequest()
            request.shouldReportPartialResults = true
            self.request = request

            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            // installTap/engine.start CRASH (uncatchable ObjC exception) on an
            // invalid format. outputFormat alone is not a safe check: on the
            // Simulator (and right after permission granting) it can report a
            // cached 44.1kHz format while the hardware-side inputFormat is
            // 0Hz/0ch — start() then throws NSException. Guard BOTH sides.
            let hardwareFormat = input.inputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0,
                  hardwareFormat.sampleRate > 0, hardwareFormat.channelCount > 0
            else {
                errorMessage = "Microphone unavailable"
                stopRecording()
                return
            }
            input.removeTap(onBus: 0)
            // The tap fires on the audio queue: it must be @Sendable, or the
            // inherited main-actor isolation SIGTRAPs on the first buffer
            // (same class of crash as the permission handlers). `append` is
            // documented thread-safe, hence the unsafe capture.
            nonisolated(unsafe) let tapRequest = request
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { @Sendable buffer, _ in
                tapRequest.append(buffer)
            }
            engine.prepare()
            try engine.start()

            // Same @Sendable requirement as the permission handlers: results
            // arrive on a speech background queue. Extract before the hop —
            // SFSpeechRecognitionResult is not Sendable.
            task = recognizer.recognitionTask(with: request) { @Sendable [weak self] result, error in
                let text = result?.bestTranscription.formattedString
                let failed = error != nil
                Task { @MainActor in
                    guard let self else { return }
                    if let text {
                        self.transcript = text
                    }
                    if failed, self.isRecording {
                        // Recognition died mid-recording (network, etc.):
                        // keep whatever transcript we have; the user's stop
                        // still commits it.
                        self.errorMessage = "Dictation interrupted"
                    }
                }
            }
            isRecording = true
        } catch {
            errorMessage = "Couldn't start the microphone"
            stopRecording()
        }
    }
    #endif

    private func stopRecording() {
        #if os(iOS)
        if let backend = modernBackend as? ModernDictationBackendStopping {
            backend.stop()
            modernBackend = nil
        }
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        request?.endAudio()
        task?.cancel()
        request = nil
        task = nil
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
        #endif
        isRecording = false
    }
}

#if os(iOS)
/// Type-erasing hook so the controller can `stop()` the modern backend without
/// an `@available` cast at the call site.
protocol ModernDictationBackendStopping: AnyObject {
    func stop()
}

/// iOS 26 on-device dictation via `SpeechAnalyzer` + `DictationTranscriber`.
/// Owns the analyzer, the audio input stream, and the results loop; converts
/// mic buffers to the analyzer's required format. All UI is delivered through
/// `onTranscript` / `onFailure` (which hop to the main actor themselves).
@available(iOS 26.0, *)
final class ModernDictationBackend: ModernDictationBackendStopping, @unchecked Sendable {
    /// Live transcript (finalized + current volatile), latest wins.
    var onTranscript: (@Sendable (String) -> Void)?
    /// `recoverable` = the modern path failed to start and the caller should
    /// fall back to the classic recognizer.
    var onFailure: (@Sendable (String, _ recoverable: Bool) -> Void)?

    private let engine: AVAudioEngine
    private var analyzer: SpeechAnalyzer?
    private var transcriber: DictationTranscriber?
    private var inputContinuation: AsyncStream<AnalyzerInput>.Continuation?
    private var resultsTask: Task<Void, Never>?
    private var runTask: Task<Void, Never>?

    init(engine: AVAudioEngine) {
        self.engine = engine
    }

    /// Returns true once audio is flowing and the results loop is live. On any
    /// setup failure it invokes `onFailure(_, recoverable: true)` and returns
    /// false so the caller can fall back.
    func start() async -> Bool {
        let locale = Locale.current
        let supported = await DictationTranscriber.supportedLocales
        // Match on language+region; fall back to language-only.
        let hasLocale = supported.contains {
            $0.identifier(.bcp47) == locale.identifier(.bcp47)
        } || supported.contains {
            $0.language.languageCode == locale.language.languageCode
        }
        guard hasLocale else {
            onFailure?("Dictation isn't set up for this language", true)
            return false
        }

        let transcriber = DictationTranscriber(
            locale: locale,
            preset: .progressiveShortDictation
        )
        self.transcriber = transcriber
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        self.analyzer = analyzer

        guard let analyzerFormat = await SpeechAnalyzer.bestAvailableAudioFormat(
            compatibleWith: [transcriber]
        ) else {
            onFailure?("Dictation model unavailable", true)
            return false
        }

        // Configure the audio session + tap. Any throw → recoverable fallback.
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.record, mode: .measurement, options: .duckOthers)
            try session.setActive(true, options: .notifyOthersOnDeactivation)
            guard session.isInputAvailable else {
                onFailure?("Microphone unavailable", true)
                return false
            }

            let input = engine.inputNode
            let inputFormat = input.outputFormat(forBus: 0)
            let hardwareFormat = input.inputFormat(forBus: 0)
            guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0,
                  hardwareFormat.sampleRate > 0, hardwareFormat.channelCount > 0
            else {
                onFailure?("Microphone unavailable", true)
                return false
            }

            let (stream, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
            self.inputContinuation = continuation

            let converter = inputFormat == analyzerFormat
                ? nil
                : AVAudioConverter(from: inputFormat, to: analyzerFormat)

            input.removeTap(onBus: 0)
            // Audio queue: @Sendable, no main-actor capture. The continuation
            // is Sendable; the converter is used single-threaded on this queue.
            nonisolated(unsafe) let unsafeConverter = converter
            input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { @Sendable buffer, _ in
                let toYield: AVAudioPCMBuffer
                if let unsafeConverter {
                    guard let converted = Self.convert(
                        buffer, using: unsafeConverter, to: analyzerFormat
                    ) else { return }
                    toYield = converted
                } else {
                    toYield = buffer
                }
                continuation.yield(AnalyzerInput(buffer: toYield))
            }
            engine.prepare()
            try engine.start()

            // Capture Sendable locals (not self) so the tasks don't reach into
            // the backend's mutable state off-actor.
            let onFailure = self.onFailure
            let onTranscript = self.onTranscript

            // Drive the analyzer over the input stream.
            runTask = Task {
                do {
                    try await analyzer.start(inputSequence: stream)
                } catch {
                    onFailure?("Dictation interrupted", false)
                }
            }

            // Consume results: accumulate finalized text, show latest volatile.
            resultsTask = Task {
                var finalized = AttributedString()
                do {
                    for try await result in transcriber.results {
                        if result.isFinal {
                            finalized += result.text
                            onTranscript?(String(finalized.characters))
                        } else {
                            let combined = finalized + result.text
                            onTranscript?(String(combined.characters))
                        }
                    }
                } catch {
                    // End of stream / cancellation is normal on stop.
                }
            }
            return true
        } catch {
            onFailure?("Couldn't start the microphone", true)
            return false
        }
    }

    func stop() {
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        inputContinuation?.finish()
        inputContinuation = nil
        // Finalize any trailing audio, then tear down.
        if let analyzer {
            Task { try? await analyzer.finalizeAndFinishThroughEndOfInput() }
        }
        resultsTask?.cancel()
        runTask?.cancel()
        resultsTask = nil
        runTask = nil
        analyzer = nil
        transcriber = nil
    }

    /// Sample-rate/format convert one mic buffer into the analyzer's format.
    private static func convert(
        _ buffer: AVAudioPCMBuffer,
        using converter: AVAudioConverter,
        to format: AVAudioFormat
    ) -> AVAudioPCMBuffer? {
        let ratio = format.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1024
        guard let output = AVAudioPCMBuffer(
            pcmFormat: format, frameCapacity: capacity
        ) else { return nil }
        var consumed = false
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in
            if consumed {
                status.pointee = .noDataNow
                return nil
            }
            consumed = true
            status.pointee = .haveData
            return buffer
        }
        if error != nil || output.frameLength == 0 { return nil }
        return output
    }
}
#endif
