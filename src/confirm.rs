//! Layer 4: type-the-device-name confirmation.

/// The typed input must exactly match the device name. No "y", no Enter-only.
pub fn matches(input: &str, device: &str) -> bool {
    input.trim() == device
}
