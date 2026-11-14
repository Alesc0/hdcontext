use crate::provider::Provider;
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::Value;
use std::{fmt, io::Read};

#[derive(Debug)]
pub struct ProviderHttpError {
    status: StatusCode,
    message: String,
}

impl fmt::Display for ProviderHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

pub fn should_stop_batch(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ProviderHttpError>()
        .is_some_and(|error| error.status == StatusCode::TOO_MANY_REQUESTS)
}

pub(super) fn http_error(
    provider: Provider,
    response: reqwest::blocking::Response,
) -> ProviderHttpError {
    let status = response.status();
    let headers = response.headers().clone();
    // Read only a bounded error body. Never display its free-form text, which can echo secrets or media.
    let mut bytes = Vec::new();
    let _ = response.take(64 * 1024).read_to_end(&mut bytes);
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    describe(provider, status, &headers, &value)
}

fn describe(
    provider: Provider,
    status: StatusCode,
    headers: &HeaderMap,
    value: &Value,
) -> ProviderHttpError {
    let error = &value["error"];
    let code = error["code"].as_str().unwrap_or("");
    let kind = error["type"].as_str().unwrap_or("");
    let guidance = if provider == Provider::OpenAi && status == StatusCode::TOO_MANY_REQUESTS {
        // Only recognized codes and our own explanations may enter a saved report or GUI message.
        match code {
            "credit_balance_exhausted" => {
                "[credit_balance_exhausted] Your API organization has no prepaid credits remaining. Check https://platform.openai.com/settings/organization/billing/overview. Retrying will not restore credits."
            }
            "organization_spend_limit_exceeded" => {
                "[organization_spend_limit_exceeded] Your organization reached its API spending limit. Review https://platform.openai.com/settings/organization/limits before retrying."
            }
            "project_spend_limit_exceeded" => {
                "[project_spend_limit_exceeded] The API project for this key reached its spending limit. Review the project's limits at https://platform.openai.com/settings/ before retrying."
            }
            "organization_usage_limit_exceeded" => {
                "[organization_usage_limit_exceeded] Your organization reached its approved API usage limit. Review https://platform.openai.com/settings/organization/limits or contact OpenAI support."
            }
            _ if code == "insufficient_quota" || kind == "insufficient_quota" => {
                "[insufficient_quota] OpenAI reports an API credit, billing, or usage-quota problem. Check the organization and project associated with this key at https://platform.openai.com/settings/organization/billing/overview and https://platform.openai.com/settings/organization/limits. ChatGPT subscriptions and API billing are separate. Waiting and regenerating will not resolve exhausted quota."
            }
            _ if error["message"]
                .as_str()
                .is_some_and(|m| m.to_ascii_lowercase().contains("request too large")) =>
            {
                "OpenAI reports that this request exceeds its rate-limit allowance. Try a shorter clip to reduce audio and image input, or review the selected models' limits at https://platform.openai.com/settings/organization/limits. Waiting alone may not make an oversized request fit."
            }
            "rate_limit_exceeded" | "slow_down" => {
                "Temporary OpenAI rate limit. Wait before retrying a single clip. Review request and token limits at https://platform.openai.com/settings/organization/limits; shorter clips can reduce token usage."
            }
            _ if kind == "rate_limit_error" => {
                "Temporary OpenAI rate limit. Wait before retrying a single clip. Review the selected models' request and token limits at https://platform.openai.com/settings/organization/limits."
            }
            _ => {
                "OpenAI did not provide a recognized limit code. This may be a temporary request/token limit or an API billing/quota limit. Check https://platform.openai.com/settings/organization/billing/overview and https://platform.openai.com/settings/organization/limits. ChatGPT subscriptions and API billing are separate."
            }
        }
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        "The provider reached a request, token, or quota limit. Review its API quota and billing before retrying a single clip."
    } else {
        "Check your API key, model access, quota, and billing."
    };
    let retry_after = if status == StatusCode::TOO_MANY_REQUESTS {
        headers.get("retry-after").and_then(|h| h.to_str().ok()).and_then(|h| h.trim().parse::<u64>().ok())
            .map(|seconds| format!(" Provider Retry-After: wait at least {seconds} seconds before retrying a temporary rate limit."))
            .unwrap_or_default()
    } else {
        String::new()
    };
    ProviderHttpError {
        status,
        message: format!(
            "{} returned HTTP {status}. {guidance}{retry_after} No automatic retry was made.",
            provider.label()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn distinguishes_quota_credit_spend_usage_and_temporary_limits() {
        for (code, expected) in [
            (
                "insufficient_quota",
                "ChatGPT subscriptions and API billing are separate",
            ),
            ("credit_balance_exhausted", "no prepaid credits"),
            (
                "organization_spend_limit_exceeded",
                "organization reached its API spending limit",
            ),
            ("project_spend_limit_exceeded", "project for this key"),
            (
                "organization_usage_limit_exceeded",
                "approved API usage limit",
            ),
            ("rate_limit_exceeded", "Temporary OpenAI rate limit"),
            ("slow_down", "Temporary OpenAI rate limit"),
        ] {
            let kind = if matches!(code, "rate_limit_exceeded" | "slow_down") {
                "requests"
            } else {
                "insufficient_quota"
            };
            let error = describe(
                Provider::OpenAi,
                StatusCode::TOO_MANY_REQUESTS,
                &HeaderMap::new(),
                &json!({"error":{"code":code,"type":kind}}),
            );
            // Specific billing codes take precedence over the broader insufficient_quota type.
            assert!(error.to_string().contains(expected), "{code}: {error}");
            assert!(should_stop_batch(
                &anyhow::Error::new(error).context("Audio analysis")
            ));
        }
        let rate = describe(
            Provider::OpenAi,
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            &json!({"error":{"code":"rate_limit_exceeded","type":"tokens"}}),
        );
        assert!(rate.to_string().contains("Temporary OpenAI rate limit"));
    }

    #[test]
    fn omits_raw_messages_unknown_codes_and_unsafe_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "sk-secret-key".parse().unwrap());
        let error = describe(
            Provider::OpenAi,
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            &json!({"error":{"code":"sk-secret-key","type":"private clip text","message":"sk-secret-key private clip text"}}),
        );
        assert!(!error.to_string().contains("sk-secret-key"));
        assert!(!error.to_string().contains("private clip text"));
        headers.insert("retry-after", "17".parse().unwrap());
        let error = describe(
            Provider::OpenAi,
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            &Value::Null,
        );
        assert!(error.to_string().contains("at least 17 seconds"));
    }

    #[test]
    fn explains_oversized_requests_without_echoing_provider_text() {
        let error = describe(
            Provider::OpenAi,
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            &json!({"error":{"code":"rate_limit_exceeded","message":"Request too large: secret account and clip details"}}),
        );
        assert!(error.to_string().contains("shorter clip"));
        assert!(!error.to_string().contains("secret account"));
        assert!(!should_stop_batch(&anyhow::anyhow!("unrelated file error")));
    }
}
