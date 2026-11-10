//! Endpoint enumeration for the device dropdowns.
//!
//! Called from the GUI thread, which initialises COM at start-up.

use wasapi::{Direction, DeviceEnumerator};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The WASAPI endpoint id. Stable across reboots, so it is what we store.
    pub id: String,
    /// Friendly name shown in the dropdown.
    pub name: String,
    /// True for the current Windows default endpoint.
    pub is_default: bool,
}

/// Lists active endpoints in one direction, default first.
pub fn list(direction: Direction) -> Result<Vec<DeviceInfo>, String> {
    let enumerator = DeviceEnumerator::new().map_err(|e| e.to_string())?;
    let default_id = enumerator
        .get_default_device(direction)
        .ok()
        .and_then(|d| d.get_id().ok());

    let collection = enumerator
        .get_device_collection(direction)
        .map_err(|e| e.to_string())?;
    let count = collection.get_nbr_devices().map_err(|e| e.to_string())?;

    let mut devices = Vec::with_capacity(count as usize);
    for index in 0..count {
        // A device can vanish between the count and the fetch; skip it quietly.
        let Ok(device) = collection.get_device_at_index(index) else {
            continue;
        };
        let Ok(id) = device.get_id() else { continue };
        let name = device
            .get_friendlyname()
            .unwrap_or_else(|_| "Unknown device".to_string());
        let is_default = default_id.as_deref() == Some(id.as_str());
        devices.push(DeviceInfo {
            id,
            name,
            is_default,
        });
    }

    devices.sort_by_key(|d| !d.is_default);
    Ok(devices)
}

/// Resolves a stored endpoint id to a device, falling back to the current
/// Windows default when that endpoint is gone (unplugged, disabled, or the
/// user changed the default while we were running).
pub fn resolve(
    enumerator: &DeviceEnumerator,
    id: Option<&str>,
    direction: Direction,
) -> Result<wasapi::Device, wasapi::WasapiError> {
    if let Some(id) = id {
        if let Ok(device) = enumerator.get_device(id) {
            return Ok(device);
        }
    }
    enumerator.get_default_device(direction)
}
