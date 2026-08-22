//! What the frontend may conclude about a device, and on what evidence.
//!
//! Two questions are kept apart on purpose, because the facts that answer them
//! are not the same strength (design.md 2.2):
//!
//! * *which profiles is this device compatible with* — answerable from a USB
//!   identity and a mode, which prove a protocol personality;
//! * *which physical board is this* — answerable only from a fact that binds a
//!   product model, which a USB vendor/product pair never is.
//!
//! Reporting the first as if it were the second is how a third-party board in
//! maskrom gets flashed with someone else's firmware. So the block this module
//! builds always carries both answers, its evidence, and its strength — never a
//! bare conclusion.

use arkforge_client::DeviceObservationView;
use arkforge_core::profile::DeviceProfile;
use arkforged::profiles;

/// Facts that bind a physical product model.
///
/// A USB descriptor string is deliberately absent: any device can claim any
/// product name, so a descriptor proves the mode or protocol at most, never the
/// board. Only a fact read over a channel the board itself has to answer on
/// belongs here.
const MODEL_BINDING_FACT_KEYS: &[&str] = &["hdc.productModel", "const.product.model"];

/// The profiles this build knows about.
pub struct ProfileRegistry {
    profiles: Vec<DeviceProfile>,
}

impl ProfileRegistry {
    /// Loads the profiles compiled into this build.
    ///
    /// A shipped profile that does not validate is a build defect, not a user
    /// error, so it is reported rather than silently skipped.
    pub fn load() -> Result<Self, String> {
        let profiles =
            profiles::shipped().map_err(|(name, error)| format!("{name} is invalid: {error}"))?;
        Ok(Self { profiles })
    }

    pub fn profiles(&self) -> &[DeviceProfile] {
        &self.profiles
    }

    pub fn find(&self, reference: &str) -> Option<&DeviceProfile> {
        self.profiles
            .iter()
            .find(|profile| profile_reference(profile) == reference)
    }

    /// Profiles whose declared artifact formats include this container format.
    pub fn compatible_with_format(&self, format_id: &str) -> Vec<String> {
        let mut references = self
            .profiles
            .iter()
            .filter(|profile| {
                profile
                    .artifact_formats
                    .iter()
                    .any(|format| format.as_str() == format_id)
            })
            .map(profile_reference)
            .collect::<Vec<_>>();
        references.sort();
        references
    }
}

/// The exact `id@major.minor.patch` reference a caller may pass to `--profile`.
pub fn profile_reference(profile: &DeviceProfile) -> String {
    format!("{}@{}", profile.id, profile.version)
}

/// How well the physical identity of a device is established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    /// Nothing in this build recognizes the device.
    None,
    /// A measured USB identity places it in one or more protocol personalities.
    UsbMode,
    /// A probe answered as well, so the mode is confirmed on the wire.
    ModeAndDeviceInfo,
    /// A model-binding fact proved which board this is.
    Strong,
}

impl Strength {
    pub fn as_str(self) -> &'static str {
        match self {
            Strength::None => "none",
            Strength::UsbMode => "usb-mode",
            Strength::ModeAndDeviceInfo => "mode+device-info",
            Strength::Strong => "strong",
        }
    }
}

/// What the frontend concluded about one observation, and why.
#[derive(Debug, Clone)]
pub struct Identification {
    /// The physical product model, when a model-binding fact proved one.
    pub model: Option<String>,
    /// The single compatible profile, when the compatible set has exactly one
    /// member. It is not a claim about which board this is.
    pub profile: Option<String>,
    pub profile_resolution: &'static str,
    pub compatible_profiles: Vec<String>,
    pub evidence: Vec<String>,
    pub strength: Strength,
}

impl Identification {
    pub fn to_json(&self, json: impl Fn(&str) -> String) -> String {
        let optional = |value: Option<&str>| value.map(&json).unwrap_or_else(|| "null".to_string());
        format!(
            "{{\"model\":{},\"profile\":{},\"profile_resolution\":{},\"compatible_profiles\":[{}],\"evidence\":[{}],\"strength\":{}}}",
            optional(self.model.as_deref()),
            optional(self.profile.as_deref()),
            json(self.profile_resolution),
            self.compatible_profiles
                .iter()
                .map(|value| json(value))
                .collect::<Vec<_>>()
                .join(","),
            self.evidence
                .iter()
                .map(|value| json(value))
                .collect::<Vec<_>>()
                .join(","),
            json(self.strength.as_str()),
        )
    }
}

/// A `(key, value)` fact pair as the probe and observation surfaces report them.
pub type Fact<'a> = (&'a str, &'a str);

/// Builds the identification block for one observation.
///
/// `probe_facts` is `Some` only when an active probe actually ran; passing an
/// empty slice would claim a probe answered with nothing, which is a different
/// fact from no probe at all.
pub fn identify(
    registry: &ProfileRegistry,
    observation: &DeviceObservationView,
    probe_facts: Option<&[Fact<'_>]>,
) -> Identification {
    let mut evidence = Vec::new();
    let usb_identity = observation
        .protocol_identity
        .iter()
        .find(|fact| fact.key == "usb.identity")
        .map(|fact| fact.value.clone());
    if let Some(identity) = &usb_identity {
        evidence.push(format!("usb-identity:{identity}"));
    }
    if !observation.mode.is_empty() {
        evidence.push(format!("usb-mode:{}", observation.mode));
    }

    let compatible = usb_identity
        .as_deref()
        .and_then(parse_usb_identity)
        .map(|(vendor_id, product_id)| {
            let mut references = registry
                .profiles()
                .iter()
                .filter(|profile| {
                    profile
                        .mode_for_usb_identity(vendor_id, product_id)
                        .is_some_and(|mode| mode.as_str() == observation.mode)
                })
                .map(profile_reference)
                .collect::<Vec<_>>();
            references.sort();
            references
        })
        .unwrap_or_default();

    let mut strength = if compatible.is_empty() {
        Strength::None
    } else {
        Strength::UsbMode
    };

    if let Some(facts) = probe_facts {
        evidence.push(format!("device-info-facts:{}", facts.len()));
        if strength == Strength::UsbMode {
            strength = Strength::ModeAndDeviceInfo;
        }
    }

    // A single compatible profile names one board only when a model-binding
    // fact says so. "The only profile this build ships that matches" is a fact
    // about the registry, not about the device on the desk.
    let mut model = None;
    if compatible.len() == 1
        && let Some(profile) = registry.find(&compatible[0])
        && let Some(facts) = probe_facts
    {
        for (key, value) in facts {
            if MODEL_BINDING_FACT_KEYS.contains(key)
                && profile
                    .product_models
                    .iter()
                    .any(|declared| declared == value)
            {
                evidence.push(format!("product-model:{key}={value}"));
                model = Some((*value).to_string());
                strength = Strength::Strong;
                break;
            }
        }
    }

    let profile_resolution = match compatible.len() {
        0 => "unrecognized",
        1 => "inferred",
        _ => "ambiguous",
    };
    Identification {
        model,
        profile: (compatible.len() == 1).then(|| compatible[0].clone()),
        profile_resolution,
        compatible_profiles: compatible,
        evidence,
        strength,
    }
}

/// Parses the `0xVVVV:0xPPPP` identity the transport reports.
fn parse_usb_identity(value: &str) -> Option<(u16, u16)> {
    let (vendor, product) = value.split_once(':')?;
    let vendor = u16::from_str_radix(vendor.trim().strip_prefix("0x")?, 16).ok()?;
    let product = u16::from_str_radix(product.trim().strip_prefix("0x")?, 16).ok()?;
    Some((vendor, product))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkforge_ipc::messages::KeyValue;

    fn observation(mode: &str, identity: &str) -> DeviceObservationView {
        DeviceObservationView {
            observation_id: "USB-2207-350a-01120000".into(),
            mode: mode.into(),
            identity_strength: "serialAndTopology".into(),
            protocol_identity: vec![
                KeyValue {
                    key: "usb.identity".into(),
                    value: identity.into(),
                },
                // A descriptor string that claims a board name. It must not
                // become a model conclusion.
                KeyValue {
                    key: "usb.productName".into(),
                    value: "DAYU200".into(),
                },
            ],
            ..DeviceObservationView::default()
        }
    }

    #[test]
    fn a_loader_usb_identity_yields_a_compatible_profile_but_never_a_model() {
        let registry = ProfileRegistry::load().unwrap();
        let identified = identify(
            &registry,
            &observation("rockusb-loader", "0x2207:0x350a"),
            None,
        );
        assert_eq!(
            identified.compatible_profiles,
            vec!["org.openharmony.dayu200@1.0.0"]
        );
        assert_eq!(
            identified.profile.as_deref(),
            Some("org.openharmony.dayu200@1.0.0")
        );
        assert_eq!(identified.profile_resolution, "inferred");
        assert_eq!(identified.model, None, "a USB pair never proves the board");
        assert_eq!(identified.strength, Strength::UsbMode);
        assert!(
            identified
                .evidence
                .contains(&"usb-identity:0x2207:0x350a".to_string())
        );
    }

    #[test]
    fn a_probe_confirms_the_mode_without_promoting_the_model() {
        let registry = ProfileRegistry::load().unwrap();
        let facts = [("transport", "usb"), ("usb.productName", "DAYU200")];
        let identified = identify(
            &registry,
            &observation("rockusb-loader", "0x2207:0x350a"),
            Some(&facts),
        );
        assert_eq!(identified.strength, Strength::ModeAndDeviceInfo);
        assert_eq!(identified.model, None);
    }

    #[test]
    fn a_model_binding_fact_is_the_only_route_to_a_strong_model() {
        let registry = ProfileRegistry::load().unwrap();
        let facts = [("hdc.productModel", "DAYU200")];
        let identified = identify(
            &registry,
            &observation("hdc-normal", "0x2207:0x5000"),
            Some(&facts),
        );
        assert_eq!(identified.model.as_deref(), Some("DAYU200"));
        assert_eq!(identified.strength, Strength::Strong);

        // A model the compatible profile does not declare is not a model.
        let facts = [("hdc.productModel", "SOMEONE-ELSES-BOARD")];
        let identified = identify(
            &registry,
            &observation("hdc-normal", "0x2207:0x5000"),
            Some(&facts),
        );
        assert_eq!(identified.model, None);
        assert_eq!(identified.strength, Strength::ModeAndDeviceInfo);
    }

    #[test]
    fn an_unrecognized_identity_concludes_nothing() {
        let registry = ProfileRegistry::load().unwrap();
        let identified = identify(
            &registry,
            &observation("rockusb-loader", "0xdead:0xbeef"),
            None,
        );
        assert!(identified.compatible_profiles.is_empty());
        assert_eq!(identified.profile, None);
        assert_eq!(identified.profile_resolution, "unrecognized");
        assert_eq!(identified.strength, Strength::None);
    }

    #[test]
    fn artifact_formats_select_the_profiles_that_declare_them() {
        let registry = ProfileRegistry::load().unwrap();
        assert_eq!(
            registry.compatible_with_format("rockchip-images-targz"),
            vec!["org.openharmony.dayu200@1.0.0"]
        );
        assert!(registry.compatible_with_format("no-such-format").is_empty());
    }
}
