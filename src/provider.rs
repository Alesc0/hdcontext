use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
pub enum Provider {
    #[default]
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "openai")]
    #[value(name = "openai")]
    OpenAi,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Gemini, Self::OpenAi];

    pub fn index(self) -> usize {
        match self {
            Self::Gemini => 0,
            Self::OpenAi => 1,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Gemini => "Google Gemini",
            Self::OpenAi => "OpenAI",
        }
    }
    pub fn key_env(self) -> &'static str {
        match self {
            Self::Gemini => "GEMINI_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
        }
    }
    pub fn model_env(self) -> &'static str {
        match self {
            Self::Gemini => "GEMINI_MODEL",
            Self::OpenAi => "OPENAI_MODEL",
        }
    }
    pub const fn default_model(self) -> &'static str {
        match self {
            Self::Gemini => "gemini-3.5-flash-lite",
            Self::OpenAi => "gpt-5.6-terra",
        }
    }
    pub fn configured_model(self) -> String {
        std::env::var(self.model_env()).unwrap_or_else(|_| self.default_model().into())
    }
}

pub const DEFAULT_AUDIO_MODEL: &str = "gpt-audio-1.5";

pub fn configured_audio_model() -> String {
    std::env::var("OPENAI_AUDIO_MODEL").unwrap_or_else(|_| DEFAULT_AUDIO_MODEL.into())
}
