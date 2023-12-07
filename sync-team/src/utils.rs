use anyhow::Context;
use reqwest::blocking::Response;
use serde::de::DeserializeOwned;

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

    fn json_annotated<T: DeserializeOwned>(self) -> anyhow::Result<T> {
        let text = self.text()?;
        serde_json::from_str::<T>(&text).with_context(|| {
            format!(
                "Cannot deserialize type `{}` from\n{}",
                std::any::type_name::<T>(),
                text
            )
        })
    }
}
