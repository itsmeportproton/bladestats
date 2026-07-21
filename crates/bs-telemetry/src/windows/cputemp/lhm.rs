//! Reading LibreHardwareMonitor's sensor tree over its own web server.
//!
//! Its WMI provider would have been the tidier route and is not available: the namespace is
//! only registered when the program runs elevated with publishing switched on, and on a
//! machine where it is simply running there is no `root\LibreHardwareMonitor` to query at all.
//! The web server is the interface that can be turned on from a settings file, which matters
//! because bladestats installs its own copy and therefore writes that file.
//!
//! Nothing here starts or configures anything. This only reads, and reads nothing if the
//! server is not there.

use std::time::Duration;

use super::CpuTempSource;

/// Where LibreHardwareMonitor serves its sensor tree by default.
pub const DEFAULT_PORT: u16 = 8085;

/// Sensor labels that mean "the processor's temperature", best first.
///
/// Ordering matters more than it looks. `Core (Tctl/Tdie)` is the control temperature AMD's
/// own tools show and the one a user will compare against; `CPU Package` is Intel's
/// equivalent. `Core Max` is the hottest individual core, which runs higher than either and is
/// a last resort rather than a synonym.
const LABELS: [&str; 5] = [
    "Core (Tctl/Tdie)",
    "CPU Package",
    "Core (Tctl)",
    "CPU Die (average)",
    "Core Max",
];

pub struct LibreHardwareMonitor {
    url: String,
}

impl LibreHardwareMonitor {
    pub fn new(port: u16) -> Self {
        Self {
            // Loopback by name rather than "localhost", which resolves through the hosts file
            // and can be made to mean something else.
            url: format!("http://127.0.0.1:{port}/data.json"),
        }
    }
}

impl Default for LibreHardwareMonitor {
    fn default() -> Self {
        Self::new(DEFAULT_PORT)
    }
}

impl CpuTempSource for LibreHardwareMonitor {
    fn name(&self) -> &'static str {
        "LibreHardwareMonitor"
    }

    fn read(&mut self) -> Option<f32> {
        let body = http::get(&self.url, Duration::from_millis(400))?;
        let tree: serde_json::Value = serde_json::from_slice(&body).ok()?;
        find_temperature(&tree)
    }
}

/// Walks the sensor tree for the best available processor temperature.
///
/// The tree is nested by machine, then component, then sensor kind, and the labels are not
/// unique across it — "Temperatures" appears under the processor and under every drive. What
/// is unique is the leaf label, which is why the search is by label and by preference rather
/// than by position.
fn find_temperature(tree: &serde_json::Value) -> Option<f32> {
    for label in LABELS {
        if let Some(c) = find_labelled(tree, label) {
            return Some(c);
        }
    }
    None
}

fn find_labelled(node: &serde_json::Value, label: &str) -> Option<f32> {
    if node.get("Text").and_then(|t| t.as_str()) == Some(label)
        && let Some(value) = node.get("Value").and_then(|v| v.as_str())
        && let Some(c) = parse_celsius(value)
    {
        return Some(c);
    }
    node.get("Children")?
        .as_array()?
        .iter()
        .find_map(|child| find_labelled(child, label))
}

/// Reads `58,0 °C` or `58.0 °C`.
///
/// The decimal separator follows the machine's locale rather than the format, because the
/// value arrives already formatted for display. On a machine set to most of Europe this is a
/// comma, and parsing it as an English number yields nothing at all — a silent blank rather
/// than a wrong number, which is the kind of failure that gets blamed on the sensor.
fn parse_celsius(text: &str) -> Option<f32> {
    let number: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-')
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    number.parse().ok()
}

/// A minimal HTTP client over WinHTTP.
///
/// One request to loopback, no redirects, no keep-alive, a short timeout. Reaching for an HTTP
/// crate for this would pull a TLS stack and a runtime into a program whose whole point is
/// being small, to talk to a program on the same machine without encryption.
mod http {
    use std::time::Duration;

    use windows::Win32::Networking::WinHttp::*;
    use windows::core::{HSTRING, PCWSTR};

    pub fn get(url: &str, timeout: Duration) -> Option<Vec<u8>> {
        let (host, port, path) = split(url)?;
        let ms = timeout.as_millis() as i32;

        unsafe {
            let session = WinHttpOpen(
                PCWSTR(HSTRING::from("bladestats").as_ptr()),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            );
            if session.is_null() {
                return None;
            }
            let session = Handle(session);
            WinHttpSetTimeouts(session.0, ms, ms, ms, ms).ok()?;

            let connect = WinHttpConnect(
                session.0,
                PCWSTR(HSTRING::from(host).as_ptr()),
                port,
                0,
            );
            if connect.is_null() {
                return None;
            }
            let connect = Handle(connect);

            let request = WinHttpOpenRequest(
                connect.0,
                PCWSTR(HSTRING::from("GET").as_ptr()),
                PCWSTR(HSTRING::from(path).as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_OPEN_REQUEST_FLAGS(0),
            );
            if request.is_null() {
                return None;
            }
            let request = Handle(request);

            WinHttpSendRequest(request.0, None, None, 0, 0, 0).ok()?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut()).ok()?;

            let mut body = Vec::new();
            loop {
                let mut available = 0u32;
                if WinHttpQueryDataAvailable(request.0, &mut available).is_err() || available == 0 {
                    break;
                }
                // A monitor's sensor tree is tens of kilobytes. Anything past a megabyte is
                // not one, and reading it would be obliging a stranger.
                if body.len() + available as usize > 1024 * 1024 {
                    break;
                }
                let start = body.len();
                body.resize(start + available as usize, 0);
                let mut read = 0u32;
                if WinHttpReadData(
                    request.0,
                    body[start..].as_mut_ptr() as *mut _,
                    available,
                    &mut read,
                )
                .is_err()
                {
                    return None;
                }
                body.truncate(start + read as usize);
                if read == 0 {
                    break;
                }
            }
            (!body.is_empty()).then_some(body)
        }
    }

    /// `http://host:port/path` — no scheme but http, since this only ever talks to loopback.
    fn split(url: &str) -> Option<(String, u16, String)> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = authority.split_once(':')?;
        Some((host.to_string(), port.parse().ok()?, format!("/{path}")))
    }

    /// Closes a WinHTTP handle however the surrounding function leaves.
    struct Handle(*mut std::ffi::c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_temperature_however_the_machine_spells_its_decimals() {
        assert_eq!(parse_celsius("58,0 °C"), Some(58.0));
        assert_eq!(parse_celsius("58.0 °C"), Some(58.0));
        assert_eq!(parse_celsius("64 °C"), Some(64.0));
        // The reason this is not just `f32::from_str`: on a machine set to most of Europe the
        // separator is a comma, and the English parse returns nothing, which reads as a
        // missing sensor rather than as a formatting problem.
        assert_eq!(parse_celsius("1234,5 RPM"), Some(1234.5));
        assert_eq!(parse_celsius(""), None);
        assert_eq!(parse_celsius("n/a"), None);
    }

    #[test]
    fn finds_the_processor_among_everything_else_in_the_tree() {
        // Shaped like the real thing: labels repeat across components, and the leaf label is
        // the only part that identifies a sensor.
        let tree: serde_json::Value = serde_json::from_str(
            r#"{
              "Text": "Sensor", "Children": [
                {"Text": "This PC", "Children": [
                  {"Text": "Samsung SSD", "Children": [
                    {"Text": "Temperatures", "Children": [
                      {"Text": "Temperature", "Value": "41,0 °C", "Children": []}]}]},
                  {"Text": "AMD Ryzen 7 9700X", "Children": [
                    {"Text": "Temperatures", "Children": [
                      {"Text": "Core (Tctl/Tdie)", "Value": "63,5 °C", "Children": []},
                      {"Text": "Core Max", "Value": "71,0 °C", "Children": []}]}]}]}]}"#,
        )
        .unwrap();

        // The control temperature, not the drive's and not the hottest single core.
        assert_eq!(find_temperature(&tree), Some(63.5));
    }

    #[test]
    fn falls_back_through_the_labels_in_order() {
        let intel: serde_json::Value = serde_json::from_str(
            r#"{"Text": "root", "Children": [
                 {"Text": "CPU Package", "Value": "55,0 °C", "Children": []},
                 {"Text": "Core Max", "Value": "72,0 °C", "Children": []}]}"#,
        )
        .unwrap();
        assert_eq!(find_temperature(&intel), Some(55.0));

        // Only the last resort available: hotter than the package reading, and better than
        // nothing, but never preferred over one.
        let sparse: serde_json::Value = serde_json::from_str(
            r#"{"Text": "root", "Children": [
                 {"Text": "Core Max", "Value": "72,0 °C", "Children": []}]}"#,
        )
        .unwrap();
        assert_eq!(find_temperature(&sparse), Some(72.0));
    }

    #[test]
    fn a_tree_without_a_processor_in_it_yields_nothing() {
        let tree: serde_json::Value =
            serde_json::from_str(r#"{"Text": "root", "Children": []}"#).unwrap();
        assert_eq!(find_temperature(&tree), None);
    }

    #[test]
    fn nothing_listening_is_silence_rather_than_an_error() {
        // The ordinary case: the monitor is not running. It must cost a refused connection and
        // a dash, not a failure the user has to read about.
        let mut source = LibreHardwareMonitor::new(1);
        assert_eq!(source.read(), None);
    }
}
