use anyhow::Context;
use reqwest::blocking::Response;
use serde::de::DeserializeOwned;
use std::str::FromStr;

pub trait ResponseExt {
    fn custom_error_for_status(self) -> anyhow::Result<Response>;
    fn json_annotated<T: DeserializeOwned>(self) -> anyhow::Result<T>;
}

impl ResponseExt for Response {
    fn custom_error_for_status(self) -> anyhow::Result<Response> {
        match self.error_for_status_ref() {
            Ok(_) => Ok(self),
            Err(err) => {
                let body = self.text()?;
                Err(err).context(format!("Body: {:?}", body))
            }
        }
    }

    /// Try to load the response as JSON. If it fails, include the response body
    /// as text in the error message, so that it is easier to understand what was
    /// the problem.
    fn json_annotated<T: DeserializeOwned>(self) -> anyhow::Result<T> {
        let text = self.text()?;

        serde_json::from_str::<T>(&text).with_context(|| {
            // Try to at least deserialize as generic JSON, to provide a more readable
            // visualization of the response body.
            let body_content = serde_json::Value::from_str(&text)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or(text);

            format!(
                "Cannot deserialize type `{}` from the following response body:\n{body_content}",
                std::any::type_name::<T>(),
            )
        })
    }
}
