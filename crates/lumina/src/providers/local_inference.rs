pub use lumina_providers::local_inference::*;

use crate::config::ExtensionConfig;
use crate::providers::api_client::TlsConfig;
use crate::providers::base::ProviderDef;
use anyhow::Result;
use futures::future::BoxFuture;

fn resolve_huggingface_token() -> BoxFuture<'static, Result<Option<String>>> {
    Box::pin(crate::providers::huggingface_auth::resolve_token_async())
}

fn resolve_string_param(key: &'static str) -> Result<Option<String>> {
    let config = crate::config::Config::global();
    if key == hf_models::MODELSCOPE_TOKEN_SECRET_KEY {
        return resolve_modelscope_credential(config);
    }
    if let Ok(value) = config.get_param::<String>(key) {
        return Ok(Some(value));
    }
    Ok(config.get_secret::<String>(key).ok())
}

fn resolve_modelscope_credential(config: &crate::config::Config) -> Result<Option<String>> {
    use crate::config::ConfigError;
    let key = hf_models::MODELSCOPE_TOKEN_SECRET_KEY;
    match config.get_param::<String>(key) {
        Ok(value) => return Ok(Some(value)),
        Err(ConfigError::NotFound(_)) => {}
        Err(_) => anyhow::bail!("ModelScope credential configuration is unavailable"),
    }
    match config.get_secret::<String>(key) {
        Ok(value) => Ok(Some(value)),
        Err(ConfigError::NotFound(_)) => Ok(None),
        Err(_) => anyhow::bail!("ModelScope credential storage is unavailable"),
    }
}

fn resolve_bool_param(key: &'static str) -> Result<Option<bool>> {
    Ok(crate::config::Config::global().get_param::<bool>(key).ok())
}

pub fn configure_local_inference() {
    huggingface_auth::set_token_resolver(resolve_huggingface_token);
    config_resolver::set_string_param_resolver(resolve_string_param);
    config_resolver::set_bool_param_resolver(resolve_bool_param);
}

pub fn configure_huggingface_auth() {
    configure_local_inference();
}

impl ProviderDef for LocalInferenceProvider {
    type Provider = Self;

    fn from_env(
        _extensions: Vec<ExtensionConfig>,
        _tls_config: Option<TlsConfig>,
    ) -> BoxFuture<'static, Result<Self::Provider>>
    where
        Self: Sized,
    {
        configure_local_inference();
        Box::pin(Self::from_env())
    }
}

#[cfg(test)]
#[path = "../../tests/local_inference/credentials.rs"]
mod credential_tests;
