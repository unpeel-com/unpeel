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
    Integration::new(Some(setup::install_gemini_hooks), None).with_resume_adapter(resume::ADAPTER);
