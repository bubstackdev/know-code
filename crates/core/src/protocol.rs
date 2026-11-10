use crate::{Error, Result};
use prost::Message;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/knowcode.rs"));

pub const VERSION: u32 = 7;
pub const INDEX_LIFECYCLE_VERSION: u32 = 1;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;
pub const ANALYSIS_POLICY_VERSION: u32 = 1;

// Validate once at the worker boundary and bind preparation reuse to the policy.
// Ordinary request construction remains legacy until shared settings are wired.
pub(crate) fn prepare_analysis_policy(
    request: &mut AnalyzeRequest,
) -> Result<Option<(u32, &'static str)>> {
    let Some(policy) = &request.analysis_policy else {
        return Ok(None);
    };
    if policy.version != ANALYSIS_POLICY_VERSION {
        return Err(Error::UpdateRequired(format!(
            "analysis policy {} is unsupported; expected {ANALYSIS_POLICY_VERSION}",
            policy.version
        )));
    }
    let language = match &policy.settings {
        Some(analysis_policy::Settings::Node(node)) => {
            if !Path::new(&node.cache_directory).is_absolute() {
                return Err(Error::InvalidRequest(
                    "package-tool cache_directory must be absolute".into(),
                ));
            }
            "typescript"
        }
        Some(analysis_policy::Settings::Csharp(context)) => {
            if context
                .configuration
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
                || context
                    .solution_path
                    .as_ref()
                    .is_some_and(|s| !Path::new(s).is_absolute())
            {
                return Err(Error::InvalidRequest(
                    "C# overrides must be nonempty and solution_path must be absolute".into(),
                ));
            }
            "csharp"
        }
        None => {
            return Err(Error::InvalidRequest(
                "analysis policy settings are required".into(),
            ));
        }
    };
    let mut hash = Sha256::new();
    hash.update(b"know-code-analysis-policy-v1\0");
    hash.update(request.preparation_fingerprint.as_bytes());
    hash.update([0]);
    hash.update(policy.encode_to_vec());
    request.preparation_fingerprint = format!("{:x}", hash.finalize());
    Ok(Some((policy.version, language)))
}

pub(crate) fn check_analysis_policy(
    requested: Option<(u32, &str)>,
    provider: &ProviderInfo,
) -> Result<()> {
    if let Some((version, language)) = requested
        && (provider.analysis_policy_version != version || provider.language != language)
    {
        return Err(Error::UpdateRequired(format!(
            "worker {} acknowledges analysis policy {}; need {language} policy {version}; update the runtime bundle",
            provider.language, provider.analysis_policy_version
        )));
    }
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Envelope>> {
    let mut header = [0; 4];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(Error::Protocol(format!(
            "frame length {size} exceeds protocol bounds"
        )));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    let envelope = Envelope::decode(bytes.as_slice())?;
    if envelope.protocol_version != VERSION {
        return Err(Error::UpdateRequired(format!(
            "protocol {} is unsupported; expected {VERSION}",
            envelope.protocol_version
        )));
    }
    Ok(Some(envelope))
}

pub fn write_frame(writer: &mut impl Write, envelope: &Envelope) -> Result<()> {
    let bytes = envelope.encode_to_vec();
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Error::Protocol(
            "outgoing frame exceeds protocol bounds".into(),
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(settings: analysis_policy::Settings) -> AnalyzeRequest {
        AnalyzeRequest {
            preparation_fingerprint: "saved-inputs".into(),
            analysis_policy: Some(AnalysisPolicy {
                version: ANALYSIS_POLICY_VERSION,
                settings: Some(settings),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn policy_presence_and_worker_acknowledgment() {
        let mut legacy = AnalyzeRequest::default();
        assert_eq!(prepare_analysis_policy(&mut legacy).unwrap(), None);
        check_analysis_policy(None, &ProviderInfo::default()).unwrap();

        let original = request(analysis_policy::Settings::Csharp(
            CSharpBuildContext::default(),
        ));
        let mut decoded = AnalyzeRequest::decode(original.encode_to_vec().as_slice()).unwrap();
        assert_eq!(decoded, original);
        let required = prepare_analysis_policy(&mut decoded).unwrap();
        let mut provider = ProviderInfo {
            language: "csharp".into(),
            ..Default::default()
        };
        assert!(matches!(
            check_analysis_policy(required, &provider),
            Err(Error::UpdateRequired(_))
        ));
        provider.analysis_policy_version = ANALYSIS_POLICY_VERSION;
        check_analysis_policy(required, &provider).unwrap();
        provider.language = "typescript".into();
        assert!(matches!(
            check_analysis_policy(required, &provider),
            Err(Error::UpdateRequired(_))
        ));
    }

    #[test]
    fn preparation_reuse_is_bound_to_policy() {
        let automatic = request(analysis_policy::Settings::Csharp(
            CSharpBuildContext::default(),
        ));
        let mut fingerprints = std::collections::BTreeSet::new();
        for mut request in [
            automatic.clone(),
            request(analysis_policy::Settings::Csharp(CSharpBuildContext {
                configuration: Some("Debug".into()),
                ..Default::default()
            })),
            request(analysis_policy::Settings::Csharp(CSharpBuildContext {
                solution_path: Some("/workspace/App.sln".into()),
                ..Default::default()
            })),
            request(analysis_policy::Settings::Node(NodePreparationPolicy {
                cache_directory: "/user/package-tools".into(),
                allow_install_scripts: false,
            })),
            request(analysis_policy::Settings::Node(NodePreparationPolicy {
                cache_directory: "/user/package-tools".into(),
                allow_install_scripts: true,
            })),
        ] {
            prepare_analysis_policy(&mut request).unwrap();
            assert!(fingerprints.insert(request.preparation_fingerprint));
        }
        let mut same = automatic;
        prepare_analysis_policy(&mut same).unwrap();
        assert!(fingerprints.contains(&same.preparation_fingerprint));
    }

    #[test]
    fn invalid_or_unknown_policy_is_rejected() {
        let mut absent = AnalyzeRequest {
            analysis_policy: Some(AnalysisPolicy {
                version: ANALYSIS_POLICY_VERSION,
                settings: None,
            }),
            ..Default::default()
        };
        assert!(matches!(
            prepare_analysis_policy(&mut absent),
            Err(Error::InvalidRequest(_))
        ));
        absent.analysis_policy.as_mut().unwrap().version += 1;
        assert!(matches!(
            prepare_analysis_policy(&mut absent),
            Err(Error::UpdateRequired(_))
        ));
        for configuration in ["", "  "] {
            let mut invalid = request(analysis_policy::Settings::Csharp(CSharpBuildContext {
                configuration: Some(configuration.into()),
                ..Default::default()
            }));
            assert!(matches!(
                prepare_analysis_policy(&mut invalid),
                Err(Error::InvalidRequest(_))
            ));
        }
        for settings in [
            analysis_policy::Settings::Node(NodePreparationPolicy::default()),
            analysis_policy::Settings::Csharp(CSharpBuildContext {
                solution_path: Some("relative.sln".into()),
                ..Default::default()
            }),
        ] {
            assert!(matches!(
                prepare_analysis_policy(&mut request(settings)),
                Err(Error::InvalidRequest(_))
            ));
        }
    }
}
