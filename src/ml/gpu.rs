//! Which graphics card the helper runs on (decision 3).
//!
//! ONNX Runtime's WebGPU provider asks Dawn for "an adapter" and never
//! says which one it got; on a machine with a discrete card, an integrated
//! one and a software rasterizer (llvmpipe) the answer depends on the
//! driver's ordering, which changes with the session the app runs in.
//! So the helper enumerates the Vulkan adapters itself (through `wgpu`,
//! which the app already carries), picks one by a fixed rule, and pins
//! Dawn to it through Mesa's device-select layer
//! (`MESA_VK_DEVICE_SELECT=vendor:device!`, read when a Vulkan instance
//! enumerates its devices, so setting it just before the session is made
//! is enough).  The adapter's name is then known honestly.
//!
//! **The rule:** a discrete card first, else an integrated one; a software
//! adapter (`llvmpipe`, SwiftShader, anything the driver calls a CPU or
//! virtual device) is never used: running "on the card" through it would
//! be processor work in disguise.  No hardware adapter means
//! [`CardFailure::NoCard`].  `PP_GPU=<part of a name>` picks a particular
//! hardware adapter for tests; it never selects a software one.

use super::CardFailure;

/// What kind of device an adapter is, as the driver reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discrete,
    Integrated,
    /// Virtual, software, or unknown: never used.
    Software,
}

/// A Vulkan adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub kind: Kind,
}

impl Adapter {
    /// The device-select layer's spelling.
    pub fn selector(&self) -> String {
        format!("{:x}:{:x}", self.vendor, self.device)
    }
}

/// Every Vulkan adapter the machine offers, in the driver's order.
pub fn enumerate() -> Vec<Adapter> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..Default::default() });
    instance
        .enumerate_adapters(wgpu::Backends::VULKAN)
        .into_iter()
        .map(|a| {
            let info = a.get_info();
            let kind = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => Kind::Discrete,
                wgpu::DeviceType::IntegratedGpu => Kind::Integrated,
                wgpu::DeviceType::Cpu | wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => Kind::Software,
            };
            Adapter { name: info.name, vendor: info.vendor, device: info.device, kind }
        })
        .collect()
}

/// The adapter to use among `adapters`: the one whose name contains
/// `wanted` (case-insensitive) when given, else the first discrete card,
/// else the first integrated one.  Software adapters are never chosen,
/// named or not.
pub fn choose<'a>(adapters: &'a [Adapter], wanted: Option<&str>) -> Result<&'a Adapter, CardFailure> {
    let hardware: Vec<&Adapter> = adapters.iter().filter(|a| a.kind != Kind::Software).collect();
    if let Some(w) = wanted.map(str::trim).filter(|w| !w.is_empty()) {
        let lower = w.to_lowercase();
        return hardware.iter().copied().find(|a| a.name.to_lowercase().contains(&lower)).ok_or_else(|| {
            CardFailure::Failed(format!(
                "PP_GPU names \"{w}\", but no graphics card matches it (found: {})",
                names(&hardware)
            ))
        });
    }
    hardware
        .iter()
        .copied()
        .find(|a| a.kind == Kind::Discrete)
        .or_else(|| hardware.first().copied())
        .ok_or(CardFailure::NoCard)
}

fn names(adapters: &[&Adapter]) -> String {
    if adapters.is_empty() { "none".into() } else { adapters.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ") }
}

/// Enumerate, choose (honouring `PP_GPU`), and pin Dawn to the choice for
/// this process.  Returns the adapter.
pub fn pin() -> Result<Adapter, CardFailure> {
    let adapters = enumerate();
    eprintln!(
        "ml-worker: graphics adapters: {}",
        adapters.iter().map(|a| format!("{} [{:?}, {}]", a.name, a.kind, a.selector())).collect::<Vec<_>>().join("; ")
    );
    let wanted = std::env::var("PP_GPU").ok();
    let chosen = choose(&adapters, wanted.as_deref())?.clone();
    // SAFETY: the helper is single-threaded at this point (sessions and the
    // reader pool come after), so nothing reads the environment concurrently.
    unsafe { std::env::set_var("MESA_VK_DEVICE_SELECT", format!("{}!", chosen.selector())) };
    Ok(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(name: &str, kind: Kind, device: u32) -> Adapter {
        Adapter { name: name.into(), vendor: 0x1002, device, kind }
    }

    #[test]
    fn a_discrete_card_is_chosen_over_an_integrated_one_and_software_never() {
        let list = vec![
            a("llvmpipe (LLVM 22.1.8, 256 bits)", Kind::Software, 0),
            a("AMD Ryzen 7 (RADV RAPHAEL_MENDOCINO)", Kind::Integrated, 0x13c0),
            a("AMD Radeon RX 7900 XTX (RADV NAVI31)", Kind::Discrete, 0x744c),
        ];
        assert_eq!(choose(&list, None).unwrap().device, 0x744c, "discrete first, whatever the order");
        assert_eq!(choose(&list[..2], None).unwrap().device, 0x13c0, "else integrated");
        assert_eq!(choose(&list[..1], None), Err(CardFailure::NoCard), "software alone is no card");
        assert_eq!(choose(&[], None), Err(CardFailure::NoCard));
        // A named adapter, by any part of its name, case aside.
        assert_eq!(choose(&list, Some("ryzen")).unwrap().device, 0x13c0);
        assert_eq!(choose(&list, Some("  7900 ")).unwrap().device, 0x744c);
        assert_eq!(choose(&list, Some("")).unwrap().device, 0x744c, "a blank name means no preference");
        assert!(matches!(choose(&list, Some("llvmpipe")), Err(CardFailure::Failed(_))), "software can't be named either");
        assert!(matches!(choose(&list, Some("nvidia")), Err(CardFailure::Failed(_))));
        assert_eq!(list[2].selector(), "1002:744c");
    }
}
