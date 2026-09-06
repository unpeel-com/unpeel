use super::Integration;

mod resume {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/gemini/adapter/resume.rs"
    ));
}

pub(crate) mod setup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../runtimes/gemini/adapter/setup.rs"
    ));
}

pub(crate) const INTEGRATION: Integration =
    Integration::new(Some(setup::install_gemini_hooks), None)
        // Gemini 0.57.0's aborted request path skips AfterAgent.
        // Its interactive useGeminiStream handler cancels on bare Escape.
        // https://geminicli.com/docs/reference/keyboard-shortcuts/
        .with_escape_cancellation()
        .with_resume_adapter(resume::ADAPTER);
