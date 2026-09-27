//! User-requested port publish intent and backend-specific translation.

use anyhow::{Result, bail};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasstPublishProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasstPublishSpec {
    pub(crate) protocol: PasstPublishProtocol,
    pub(crate) payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TsiPublishMapping {
    host: u16,
    guest: u16,
}

impl TsiPublishMapping {
    /// libkrun ABI 2's `krun_vsock_device_add_port_forward` spells a mapping
    /// `guest:host`, the reverse of the v1 `krun_set_port_map` entry.
    fn libkrun_port_forward(self) -> String {
        format!("{}:{}", self.guest, self.host)
    }

    fn pasta_tcp_forward(self) -> String {
        format!("{}:{}", self.host, self.host)
    }
}

fn tsi_publish_mappings(specs: &[String]) -> Result<Vec<TsiPublishMapping>> {
    let mut host_ports = HashSet::new();
    let mut guest_ports = HashSet::new();
    let mut mappings = Vec::with_capacity(specs.len());

    for spec in specs {
        let spec = spec.trim();
        let (host, guest) = spec.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "TSI publish spec '{spec}' must use simple HOST_PORT:GUEST_PORT TCP syntax"
            )
        })?;
        if guest.contains(':') {
            bail!("TSI publish spec '{spec}' must contain exactly one ':' separator");
        }

        let host = parse_tsi_port("host", host, spec)?;
        let guest = parse_tsi_port("guest", guest, spec)?;
        if !host_ports.insert(host) {
            bail!("TSI publish spec '{spec}' duplicates host port {host}");
        }
        if !guest_ports.insert(guest) {
            bail!("TSI publish spec '{spec}' duplicates guest port {guest}");
        }
        mappings.push(TsiPublishMapping { host, guest });
    }

    Ok(mappings)
}

pub(crate) fn tsi_port_forwards(specs: &[String]) -> Result<Vec<String>> {
    Ok(tsi_publish_mappings(specs)?
        .into_iter()
        .map(TsiPublishMapping::libkrun_port_forward)
        .collect())
}

pub(crate) fn tsi_pasta_tcp_forwards(specs: &[String]) -> Result<Vec<String>> {
    Ok(tsi_publish_mappings(specs)?
        .into_iter()
        .map(TsiPublishMapping::pasta_tcp_forward)
        .collect())
}

pub(crate) fn passt_publish_specs(specs: &[String]) -> Result<Vec<PasstPublishSpec>> {
    specs
        .iter()
        .map(|spec| {
            let spec = spec.trim();
            if spec.is_empty() {
                bail!("passt publish spec must not be empty");
            }

            if let Some((selector, payload)) = spec.split_once(':') {
                let selector_lc = selector.to_ascii_lowercase();
                let protocol = match selector_lc.as_str() {
                    "tcp" => Some(PasstPublishProtocol::Tcp),
                    "udp" => Some(PasstPublishProtocol::Udp),
                    _ => None,
                };
                if let Some(protocol) = protocol {
                    let payload = payload.trim();
                    if payload.is_empty() {
                        bail!("passt publish spec '{spec}' has an empty {selector_lc} payload");
                    }
                    return Ok(PasstPublishSpec {
                        protocol,
                        payload: payload.to_owned(),
                    });
                }

                if looks_like_protocol_selector(selector) {
                    bail!(
                        "passt publish spec '{spec}' has unsupported protocol selector '{selector}'"
                    );
                }
            }

            Ok(PasstPublishSpec {
                protocol: PasstPublishProtocol::Tcp,
                payload: spec.to_owned(),
            })
        })
        .collect()
}

fn parse_tsi_port(label: &str, value: &str, spec: &str) -> Result<u16> {
    let port = value.parse::<u16>().map_err(|_| {
        anyhow::anyhow!("TSI publish spec '{spec}' has invalid {label} port '{value}'")
    })?;
    if port == 0 {
        bail!("TSI publish spec '{spec}' cannot use {label} port 0");
    }
    Ok(port)
}

fn looks_like_protocol_selector(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == '-' || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn tsi_accepts_simple_tcp_port_maps() {
        // A publish spec is host:guest; libkrun ABI 2 wants guest:host.
        assert_eq!(
            tsi_port_forwards(&strings(&["1:65535", "65535:1"])).expect("valid port map"),
            ["65535:1", "1:65535"]
        );
    }

    #[test]
    fn tsi_derives_pasta_forwards_for_host_ports() {
        assert_eq!(
            tsi_pasta_tcp_forwards(&strings(&["8080:80", "8443:443"]))
                .expect("valid pasta forwards"),
            ["8080:8080", "8443:8443"]
        );
    }

    #[test]
    fn tsi_rejects_unsupported_or_malformed_specs() {
        for spec in [
            "8080",
            "8080:80:extra",
            "65536:80",
            "8080:65536",
            ":80",
            "8080:",
            "::80",
            "tcp:8080:80",
            "udp:5353:5353",
            "127.0.0.1:8080:80",
            "8080-8082:80-82",
            "8080:80/udp",
            "0:80",
            "8080:0",
            "all",
            "none",
        ] {
            assert!(
                tsi_port_forwards(&strings(&[spec])).is_err(),
                "{spec} should fail"
            );
        }
    }

    #[test]
    fn tsi_rejects_duplicate_host_or_guest_ports() {
        assert!(tsi_port_forwards(&strings(&["8080:80", "8080:81"])).is_err());
        assert!(tsi_port_forwards(&strings(&["8080:80", "8081:80"])).is_err());
    }

    #[test]
    fn passt_classifies_unprefixed_as_tcp_and_prefixed_specs() {
        let specs = passt_publish_specs(&strings(&["8080:80", "tcp:8443:443", "udp:5353:5353"]))
            .expect("passt specs");

        assert_eq!(
            specs,
            [
                PasstPublishSpec {
                    protocol: PasstPublishProtocol::Tcp,
                    payload: "8080:80".to_owned()
                },
                PasstPublishSpec {
                    protocol: PasstPublishProtocol::Tcp,
                    payload: "8443:443".to_owned()
                },
                PasstPublishSpec {
                    protocol: PasstPublishProtocol::Udp,
                    payload: "5353:5353".to_owned()
                }
            ]
        );
    }

    #[test]
    fn passt_preserves_broad_payload_syntax() {
        let specs = passt_publish_specs(&strings(&[
            "10000-10010:80-90",
            "8080:80/127.0.0.1",
            "8443:443%eth0",
            "udp:5353~5354:5353",
        ]))
        .expect("passt specs");

        assert_eq!(specs[0].payload, "10000-10010:80-90");
        assert_eq!(specs[1].payload, "8080:80/127.0.0.1");
        assert_eq!(specs[2].payload, "8443:443%eth0");
        assert_eq!(specs[3].payload, "5353~5354:5353");
    }

    #[test]
    fn passt_rejects_empty_payloads_and_unknown_selectors() {
        assert!(passt_publish_specs(&strings(&["tcp:"])).is_err());
        assert!(passt_publish_specs(&strings(&["udp:   "])).is_err());
        assert!(passt_publish_specs(&strings(&["sctp:5000:5000"])).is_err());
    }
}
