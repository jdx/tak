//! The machine a run was measured on, for `tak run --export-json`.
//!
//! A comparison page has to say what it was measured on: the same two tools
//! can swap places between a 4-core laptop and a 64-core server. Without this
//! every consumer of the export shells out to `lscpu`, reads `/proc/meminfo`
//! and asks for the CPU affinity itself, and gets it subtly different from
//! the next one.
//!
//! This is description, not identity. Series are partitioned on the runner
//! class ([`crate::record::Record::runner`]), and none of this goes into
//! recorded notes: a kernel update would otherwise split a series that the
//! runner class deliberately keeps whole.
//!
//! Every field is best effort. A value tak cannot read is `None` — exported as
//! `null` — never an error: a run that measured correctly must not fail over
//! how to describe the CPU. Nothing here spawns a process; it reads files on
//! Linux and asks the kernel directly on macOS and Windows.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Machine {
    /// As Rust names it: `linux`, `macos`, `windows`.
    pub os: String,
    /// The distribution or product version: `/etc/os-release`'s
    /// `PRETTY_NAME` on Linux, `15.3.1` on macOS. `None` on Windows, whose
    /// registry names Windows 11 as Windows 10; `kernel` has the build.
    pub os_version: Option<String>,
    /// Kernel release: `6.8.0-45-generic` on Linux, the Darwin release on
    /// macOS, `10.0.<build>` on Windows.
    pub kernel: Option<String>,
    /// As Rust names it: `x86_64`, `aarch64`.
    pub arch: String,
    /// The CPU's model name, as the vendor brands it.
    pub cpu: Option<String>,
    /// Logical CPUs tak — and so every subject — could run on. Not the
    /// machine's total: on Linux this honours the CPU affinity mask (`taskset`,
    /// a container's cpuset) and a cgroup CPU quota, which is what bounds a
    /// parallel tool's speed-up.
    pub cpus: Option<usize>,
    /// Total physical memory in bytes.
    pub memory_bytes: Option<u64>,
}

/// Describe this machine.
pub fn detect() -> Machine {
    let os = imp::detect();
    Machine {
        os: std::env::consts::OS.to_string(),
        os_version: os.os_version,
        kernel: os.kernel,
        arch: std::env::consts::ARCH.to_string(),
        cpu: os.cpu,
        cpus: std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZero::get),
        memory_bytes: os.memory_bytes,
    }
}

/// What each platform has to find for itself.
#[derive(Debug, Default)]
struct Os {
    os_version: Option<String>,
    kernel: Option<String>,
    cpu: Option<String>,
    memory_bytes: Option<u64>,
}

/// Trimmed, and `None` when that leaves nothing: an empty string would read
/// as a value to anything consuming the export.
fn clean(s: &str) -> Option<String> {
    let s = s.trim().trim_matches(char::from(0)).trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(any(target_os = "linux", test))]
mod linux_parse {
    //! Parsers for the Linux files, compiled everywhere so their tests run on
    //! every host.

    use super::clean;

    /// The first `model name` in `/proc/cpuinfo`. x86 has one per logical
    /// CPU; arm64 kernels usually print none, and then the CPU is unknown
    /// rather than guessed from implementer and part numbers.
    pub fn cpu_model(cpuinfo: &str) -> Option<String> {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == "model name").then(|| clean(v)).flatten()
        })
    }

    /// `MemTotal` from `/proc/meminfo`, which the kernel reports in KiB
    /// whatever the unit says.
    pub fn mem_total(meminfo: &str) -> Option<u64> {
        let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kib: u64 = line["MemTotal:".len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        kib.checked_mul(1024)
    }

    /// `PRETTY_NAME` from os-release, unquoted.
    pub fn pretty_name(os_release: &str) -> Option<String> {
        os_release.lines().find_map(|l| {
            let v = l.strip_prefix("PRETTY_NAME=")?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            clean(v)
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Os, clean, linux_parse};

    pub(super) fn detect() -> Os {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        Os {
            // /etc/os-release is the standard; /usr/lib/os-release is where
            // it points on systems that keep /etc minimal.
            os_version: read("/etc/os-release")
                .or_else(|| read("/usr/lib/os-release"))
                .and_then(|t| linux_parse::pretty_name(&t)),
            kernel: read("/proc/sys/kernel/osrelease").and_then(|t| clean(&t)),
            cpu: read("/proc/cpuinfo").and_then(|t| linux_parse::cpu_model(&t)),
            memory_bytes: read("/proc/meminfo").and_then(|t| linux_parse::mem_total(&t)),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Os, clean};
    use std::ffi::CStr;

    /// A string sysctl, read with `sysctlbyname` rather than by spawning
    /// `sysctl`, which is not on every PATH.
    fn string(name: &CStr) -> Option<String> {
        let mut len: libc::size_t = 0;
        // SAFETY: a null buffer asks only for the length.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` holds `len` bytes, and sysctl writes at most that.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        clean(&String::from_utf8_lossy(&buf))
    }

    fn u64_value(name: &CStr) -> Option<u64> {
        let mut v: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: `v` is exactly the `len` bytes sysctl is told it may write.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&raw mut v).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == std::mem::size_of::<u64>()).then_some(v)
    }

    pub(super) fn detect() -> Os {
        Os {
            os_version: string(c"kern.osproductversion"),
            kernel: string(c"kern.osrelease"),
            cpu: string(c"machdep.cpu.brand_string"),
            memory_bytes: u64_value(c"hw.memsize"),
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::{Os, clean};
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn reg_string(key: &str, value: &str) -> Option<String> {
        let (key, value) = (wide(key), wide(value));
        let mut buf = [0u16; 512];
        let mut len = std::mem::size_of_val(&buf) as u32;
        // SAFETY: both names are NUL-terminated, and `len` is `buf`'s size in
        // bytes, which RegGetValueW will not write past.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return None;
        }
        let chars = (len as usize / 2).min(buf.len());
        clean(&String::from_utf16_lossy(&buf[..chars]))
    }

    fn reg_dword(key: &str, value: &str) -> Option<u32> {
        let (key, value) = (wide(key), wide(value));
        let mut v: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        // SAFETY: as above, with `v` as the four-byte buffer.
        let rc = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut v).cast(),
                &mut len,
            )
        };
        (rc == 0).then_some(v)
    }

    pub(super) fn detect() -> Os {
        const CPU: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
        const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let kernel = match (
            reg_dword(NT, "CurrentMajorVersionNumber"),
            reg_dword(NT, "CurrentMinorVersionNumber"),
            reg_string(NT, "CurrentBuildNumber"),
        ) {
            (Some(major), Some(minor), Some(build)) => Some(format!("{major}.{minor}.{build}")),
            _ => None,
        };
        // SAFETY: MEMORYSTATUSEX is plain data; dwLength must be set first.
        let memory_bytes = unsafe {
            let mut m: MEMORYSTATUSEX = std::mem::zeroed();
            m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            (GlobalMemoryStatusEx(&mut m) != 0).then_some(m.ullTotalPhys)
        };
        Os {
            os_version: None,
            kernel,
            cpu: reg_string(CPU, "ProcessorNameString"),
            memory_bytes,
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::Os;

    pub(super) fn detect() -> Os {
        Os::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs on every host CI has: whatever can and cannot be read, detection
    /// returns, and what it returns is plausible.
    #[test]
    fn detection_describes_this_host_without_failing() {
        let m = detect();
        assert_eq!(m.os, std::env::consts::OS);
        assert_eq!(m.arch, std::env::consts::ARCH);
        assert!(m.cpus.is_some_and(|n| n >= 1), "{m:?}");
        for s in [&m.os_version, &m.kernel, &m.cpu].into_iter().flatten() {
            assert_eq!(s.trim(), s, "trimmed: {m:?}");
            assert!(!s.is_empty(), "empty is None, not a value: {m:?}");
        }
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            // More than 64 MiB and less than 64 PiB: a unit mistake either
            // way lands outside.
            let mem = m.memory_bytes.expect("memory is readable here");
            assert!((64 << 20..64 << 50).contains(&mem), "{mem}");
            assert!(m.kernel.is_some(), "{m:?}");
        }
        if cfg!(target_os = "macos") {
            assert!(m.cpu.is_some() && m.os_version.is_some(), "{m:?}");
        }
    }

    #[test]
    fn cpuinfo_model_name_is_the_first_one() {
        let x86 = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\n\nprocessor\t: 1\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\n";
        assert_eq!(
            linux_parse::cpu_model(x86).as_deref(),
            Some("AMD Ryzen 9 7950X 16-Core Processor")
        );
        let arm = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n";
        assert_eq!(linux_parse::cpu_model(arm), None);
    }

    #[test]
    fn meminfo_total_is_bytes() {
        let m = "MemTotal:       65536000 kB\nMemFree:         1234 kB\n";
        assert_eq!(linux_parse::mem_total(m), Some(65_536_000 * 1024));
        assert_eq!(linux_parse::mem_total("MemFree: 1 kB\n"), None);
        assert_eq!(linux_parse::mem_total("MemTotal: lots\n"), None);
    }

    #[test]
    fn os_release_pretty_name_is_unquoted() {
        let t = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(
            linux_parse::pretty_name(t).as_deref(),
            Some("Ubuntu 24.04.1 LTS")
        );
        assert_eq!(
            linux_parse::pretty_name("PRETTY_NAME=Arch\n").as_deref(),
            Some("Arch")
        );
        assert_eq!(linux_parse::pretty_name("PRETTY_NAME=\"\"\n"), None);
        assert_eq!(linux_parse::pretty_name("ID=alpine\n"), None);
    }

    #[test]
    fn a_missing_value_is_null_in_the_export() {
        let m = Machine {
            os: "linux".into(),
            os_version: None,
            kernel: None,
            arch: "x86_64".into(),
            cpu: None,
            cpus: None,
            memory_bytes: None,
        };
        let v = serde_json::to_value(&m).unwrap();
        assert!(v["cpu"].is_null() && v["memory_bytes"].is_null(), "{v}");
        assert_eq!(v.as_object().unwrap().len(), 7, "every key present");
    }
}
