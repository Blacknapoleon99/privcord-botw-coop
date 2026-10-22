//! BotW multiplayer graphics: which community graphic-pack presets each PC gets, and the Cemu 1.26.2 settings.xml
//! entries for them.
//!
//! Cemu 1.26.2 (the version the BOTWM mod needs) only applies graphic packs with patches up to rules `version = 6`; the
//! community repo moved BotW's Graphics pack to version 7 (Cemu 2.0's patch format) on 2021-12-26. So the packs come
//! from the last release before that, **Github833** (2021-12-23), pinned by SHA-256 and downloaded from the official
//! GitHub release at install time (PrivCord never ships them). FPS++ and Extended Memory keep the copies the app
//! already embeds (a later, still version-6 FPS++ that is proven on 1.26.2).
//!
//! The FPS cap follows the screen: 120 on 240/120 Hz screens, 144 on 144/165 Hz, otherwise 60. Never unlimited.

use serde::{Deserialize, Serialize};

/// The pinned community graphic pack release.
pub const PACKS_TAG: &str = "Github833";
pub const PACKS_URL: &str = "https://github.com/cemu-project/cemu_graphic_packs/releases/download/Github833/graphicPacks833.zip";
pub const PACKS_SHA256: &str = "d6b2ed11e64d11d7921e4cd504ccdc5212d248fa4d5517d4dc03d3cdecffe4d0";
/// Folders taken from the release (relative to its `BreathOfTheWild/`), installed under
/// `graphicPacks/downloadedGraphicPacks/BreathOfTheWild/`.
pub const PACK_DIRS: [&str; 3] = ["Graphics", "Enhancements", "Mods/DrawDistance"];

/// rules.txt paths as Cemu 1.26.2 writes them in settings.xml.
pub const GRAPHICS: &str = r"graphicPacks\downloadedGraphicPacks\BreathOfTheWild\Graphics\rules.txt";
pub const ENHANCEMENTS: &str = r"graphicPacks\downloadedGraphicPacks\BreathOfTheWild\Enhancements\rules.txt";
pub const DRAW_DISTANCE: &str = r"graphicPacks\downloadedGraphicPacks\BreathOfTheWild\Mods\DrawDistance\rules.txt";
pub const FPS_PLUS_PLUS: &str = r"graphicPacks\downloadedGraphicPacks\BreathOfTheWild\Mods\FPS++\rules.txt";

/// Quality level. `Auto` picks one from the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    #[default]
    Auto,
    Max,
    High,
    Balanced,
}

/// FPS cap. `Auto` follows the screen's refresh rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FpsCap {
    #[default]
    Auto,
    Fps60,
    Fps120,
    Fps144,
}

/// The player's choices (saved in the BotwMp install folder).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GraphicsChoice {
    pub quality: Quality,
    pub fps: FpsCap,
}

/// What this PC has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub gpu: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
}

/// The quality tier for a GPU name. Big desktop cards get Max; laptop and mid-range cards get High; the rest Balanced.
pub fn tier_for_gpu(gpu: &str) -> Quality {
    let g = gpu.to_ascii_lowercase();
    let laptop = g.contains("laptop") || g.contains("mobile") || g.contains("max-q");
    let has = |s: &[&str]| s.iter().any(|m| g.contains(m));
    if !laptop && has(&["rtx 5090", "rtx 5080", "rtx 4090", "rtx 4080", "rtx 3090", "rx 7900", "rx 9070"]) {
        Quality::Max
    } else if has(&["rtx 50", "rtx 40", "rtx 30", "rtx 2080", "rx 7", "rx 6800", "rx 6900", "rx 9"]) {
        Quality::High
    } else {
        Quality::Balanced
    }
}

/// The FPS++ "Framerate Limit" preset (Normal mode) for a cap, or for a refresh rate when the cap is `Auto`.
pub fn fps_preset(cap: FpsCap, refresh_hz: u32) -> &'static str {
    const P60: &str = "60FPS (ideal for 240/120/60Hz displays)";
    const P120: &str = "120FPS (ideal for 240/120/60Hz displays)";
    const P144: &str = "144FPS (ideal for 144Hz displays)";
    match cap {
        FpsCap::Fps60 => P60,
        FpsCap::Fps120 => P120,
        FpsCap::Fps144 => P144,
        FpsCap::Auto => {
            // Screens report 59/119/143/239 Hz for their nominal rate.
            let hz = (refresh_hz + 2) / 5 * 5;
            if hz >= 120 && hz % 120 == 0 {
                P120
            } else if hz >= 144 {
                P144
            } else if hz >= 120 {
                P120
            } else {
                P60
            }
        }
    }
}

/// Aspect-ratio preset of the Graphics pack for a screen, with its (w, h) numbers.
fn aspect(width: u32, height: u32) -> (&'static str, u32, u32) {
    let r = f64::from(width) / f64::from(height.max(1));
    if (r - 1.6).abs() < 0.02 {
        ("16:10", 16, 10)
    } else if r > 2.2 {
        ("21:9", 21, 9)
    } else {
        ("16:9 (Default)", 16, 9)
    }
}

/// Resolution preset of the Graphics pack. Max renders above the screen (supersampling) where a preset exists.
fn resolution(quality: Quality, width: u32, height: u32) -> &'static str {
    let (_, aw, ah) = aspect(width, height);
    match (aw, ah) {
        (16, 10) => match quality {
            Quality::Max if height <= 1600 => "3840x2400",
            Quality::Balanced => "1920x1200",
            _ if height >= 2400 => "3840x2400",
            _ if height >= 1600 => "2560x1600",
            _ => "1920x1200",
        },
        (21, 9) => {
            if height >= 1600 || quality == Quality::Max {
                "3840x1600"
            } else {
                "2560x1080"
            }
        }
        _ => match quality {
            Quality::Max if height <= 1440 => "3840x2160 (4K)",
            _ if height >= 2160 => "3840x2160 (4K)",
            _ if height >= 1440 => "2560x1440 (2K)",
            Quality::Balanced => "1920x1080 (Full HD)",
            _ if height >= 1080 => "1920x1080 (Full HD)",
            _ => "1280x720 (HD, Default)",
        },
    }
}

/// One graphic pack with its chosen presets: (settings.xml filename, [(category, preset)]).
pub type PackPresets = (&'static str, Vec<(&'static str, &'static str)>);

/// Presets for every pack, for this machine and choice. Returns the resolved quality too.
pub fn plan(m: &Machine, choice: GraphicsChoice) -> (Quality, Vec<PackPresets>) {
    let q = match choice.quality {
        Quality::Auto => tier_for_gpu(&m.gpu),
        q => q,
    };
    let (ar, _, _) = aspect(m.width, m.height);
    let (shadows, aniso, reflections, dd_npc, dd_world, lod) = match q {
        Quality::Max | Quality::Auto => (
            "Ultra (300%)",
            "Extreme (16x)",
            "Enhanced Reflections",
            "Ultra (1.5x)",
            "Ultra (1.5x, requires Extended Memory pack!)",
            "Higher (-2)",
        ),
        Quality::High => ("High (200%)", "Extreme (16x)", "Enhanced Reflections", "High (1.25x)", "High (1.25x)", "Slightly higher (-1)"),
        Quality::Balanced => (
            "Medium (100%, Default)",
            "Ultra (8x, Default)",
            "Normal Reflections",
            "Medium (1x)",
            "Medium (1x)",
            "Normal (Default)",
        ),
    };
    let packs = vec![
        (
            GRAPHICS,
            vec![
                ("Aspect Ratio", ar),
                ("Resolution", resolution(q, m.width, m.height)),
                ("Anti-Aliasing", "Normal FXAA (Default)"),
                ("Shadows", shadows),
            ],
        ),
        (ENHANCEMENTS, vec![("Anisotropic Filtering", aniso), ("Reflections", reflections)]),
        (
            DRAW_DISTANCE,
            vec![
                ("NPC, Enemies And Other Entities", dd_npc),
                ("Trees, Bushes, Landscape And Other Objects", dd_world),
                ("Texture Distance Detail (LOD)", lod),
            ],
        ),
        (FPS_PLUS_PLUS, vec![("Mode", "Normal Settings"), ("Framerate Limit", fps_preset(choice.fps, m.refresh_hz))]),
    ];
    (q, packs)
}

/// Rewrite a Cemu 1.26.2 settings.xml for this machine and choice: presets for the community packs (only when they are
/// installed) and FPS++, Vulkan, and async shader compilation (shaders build in the background instead of freezing the
/// game). Returns the new xml and the quality used.
pub fn apply_to_xml(xml: &str, m: &Machine, choice: GraphicsChoice, have_packs: bool) -> (String, Quality) {
    let (quality, packs) = plan(m, choice);
    let mut out = xml.to_owned();
    for (file, presets) in &packs {
        if *file == FPS_PLUS_PLUS || have_packs {
            out = upsert_entry(&out, file, presets);
        }
    }
    out = set_value(&out, "Graphic", "api", "1");
    out = set_value(&out, "Graphic", "AsyncCompile", "true");
    (out, quality)
}

/// This PC's best GPU and its primary screen's current mode. Hybrid laptops list the integrated GPU first, so the
/// best-tier adapter wins. Must run in the user's desktop session (an SSH session sees no display).
#[cfg(windows)]
pub fn detect_machine() -> Option<Machine> {
    use windows_sys::Win32::Graphics::Gdi::{DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsW};
    let wstr = |w: &[u16]| String::from_utf16_lossy(&w[..w.iter().position(|&c| c == 0).unwrap_or(w.len())]);
    let mut gpus = Vec::new();
    for i in 0..16 {
        let mut d = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
        // SAFETY: d is a properly sized DISPLAY_DEVICEW.
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut d, 0) } == 0 {
            break;
        }
        gpus.push(wstr(&d.DeviceString));
    }
    let rank = |g: &String| match tier_for_gpu(g) {
        Quality::Max => 3,
        Quality::High => 2,
        _ => 1,
    };
    let gpu = gpus.into_iter().max_by_key(rank)?;
    // SAFETY: DEVMODEW is plain data; dmSize tells the API its size.
    let mut dm: DEVMODEW = unsafe { std::mem::zeroed() };
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    if unsafe { EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) } == 0 {
        return None;
    }
    Some(Machine { gpu, width: dm.dmPelsWidth, height: dm.dmPelsHeight, refresh_hz: dm.dmDisplayFrequency })
}

#[cfg(not(windows))]
pub fn detect_machine() -> Option<Machine> {
    None
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Put an `<Entry filename="…">` with these presets into the `<GraphicPack>` section of a Cemu settings.xml, replacing
/// any entry for the same file (self-closing or with presets). Other entries are untouched.
pub fn upsert_entry(xml: &str, filename: &str, presets: &[(&str, &str)]) -> String {
    let attr = format!("filename=\"{}\"", xml_escape(filename));
    let mut entry = format!("        <Entry {attr}>\n");
    for (c, p) in presets {
        entry.push_str(&format!(
            "            <Preset>\n                <category>{}</category>\n                <preset>{}</preset>\n            </Preset>\n",
            xml_escape(c),
            xml_escape(p)
        ));
    }
    entry.push_str("        </Entry>\n");

    // Remove an existing entry for this file.
    let mut out = xml.to_owned();
    if let Some(start) = out.find(&format!("<Entry {attr}")) {
        let line_start = out[..start].rfind('\n').map_or(0, |i| i + 1);
        let tag_end = start + out[start..].find('>').unwrap_or(0);
        let end = if out[..=tag_end].ends_with("/>") {
            tag_end + 1
        } else {
            out[start..].find("</Entry>").map_or(tag_end + 1, |i| start + i + "</Entry>".len())
        };
        let end = out[end..].find('\n').map_or(out.len(), |i| end + i + 1);
        out.replace_range(line_start..end, "");
    }
    if let Some(close) = out.find("</GraphicPack>") {
        let line_start = out[..close].rfind('\n').map_or(close, |i| i + 1);
        out.insert_str(line_start, &entry);
    } else if let Some(i) = out.find("<GraphicPack/>") {
        out.replace_range(i..i + "<GraphicPack/>".len(), &format!("<GraphicPack>\n{entry}    </GraphicPack>"));
    } else if let Some(i) = out.find("</content>") {
        out.insert_str(i, &format!("    <GraphicPack>\n{entry}    </GraphicPack>\n"));
    }
    out
}

/// Set a simple `<tag>value</tag>` inside `<section>` (e.g. Graphic/api), adding it if missing.
pub fn set_value(xml: &str, section: &str, tag: &str, value: &str) -> String {
    let (open, close) = (format!("<{section}>"), format!("</{section}>"));
    let (Some(s), Some(e)) = (xml.find(&open), xml.find(&close)) else { return xml.to_owned() };
    let body = &xml[s..e];
    let (t_open, t_close) = (format!("<{tag}>"), format!("</{tag}>"));
    if let (Some(a), Some(b)) = (body.find(&t_open), body.find(&t_close)) {
        let mut out = xml.to_owned();
        out.replace_range(s + a + t_open.len()..s + b, value);
        out
    } else {
        let mut out = xml.to_owned();
        let line_start = out[..e].rfind('\n').map_or(e, |i| i + 1);
        out.insert_str(line_start, &format!("        {t_open}{value}{t_close}\n"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_pc() -> Machine {
        Machine { gpu: "NVIDIA GeForce RTX 5090".into(), width: 2560, height: 1440, refresh_hz: 239 }
    }
    fn strix() -> Machine {
        Machine { gpu: "NVIDIA GeForce RTX 4080 Laptop GPU".into(), width: 2560, height: 1600, refresh_hz: 60 }
    }

    #[test]
    fn tiers() {
        assert_eq!(tier_for_gpu("NVIDIA GeForce RTX 5090"), Quality::Max);
        assert_eq!(tier_for_gpu("NVIDIA GeForce RTX 4080 Laptop GPU"), Quality::High);
        assert_eq!(tier_for_gpu("NVIDIA GeForce RTX 4060"), Quality::High);
        assert_eq!(tier_for_gpu("NVIDIA GeForce GTX 1060"), Quality::Balanced);
        assert_eq!(tier_for_gpu("Intel(R) Iris(R) Xe Graphics"), Quality::Balanced);
    }

    #[test]
    fn fps_follows_the_screen_but_never_unlimited() {
        assert_eq!(fps_preset(FpsCap::Auto, 239), "120FPS (ideal for 240/120/60Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 144), "144FPS (ideal for 144Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 143), "144FPS (ideal for 144Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 165), "144FPS (ideal for 144Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 120), "120FPS (ideal for 240/120/60Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 60), "60FPS (ideal for 240/120/60Hz displays)");
        assert_eq!(fps_preset(FpsCap::Auto, 59), "60FPS (ideal for 240/120/60Hz displays)");
        assert_eq!(fps_preset(FpsCap::Fps144, 60), "144FPS (ideal for 144Hz displays)");
    }

    #[test]
    fn plans_for_both_pcs() {
        let (q, p) = plan(&main_pc(), GraphicsChoice::default());
        assert_eq!(q, Quality::Max);
        assert_eq!(p[0].1, vec![("Aspect Ratio", "16:9 (Default)"), ("Resolution", "3840x2160 (4K)"), ("Anti-Aliasing", "Normal FXAA (Default)"), ("Shadows", "Ultra (300%)")]);
        assert_eq!(p[3].1[1].1, "120FPS (ideal for 240/120/60Hz displays)");

        let (q, p) = plan(&strix(), GraphicsChoice::default());
        assert_eq!(q, Quality::High);
        assert_eq!(p[0].1[0], ("Aspect Ratio", "16:10"));
        assert_eq!(p[0].1[1], ("Resolution", "2560x1600"));
        assert_eq!(p[0].1[3], ("Shadows", "High (200%)"));
        assert_eq!(p[3].1[1].1, "60FPS (ideal for 240/120/60Hz displays)");

        let (q, p) = plan(&strix(), GraphicsChoice { quality: Quality::Balanced, fps: FpsCap::Fps120 });
        assert_eq!(q, Quality::Balanced);
        assert_eq!(p[0].1[1], ("Resolution", "1920x1200"));
        assert_eq!(p[3].1[1].1, "120FPS (ideal for 240/120/60Hz displays)");
    }

    #[test]
    fn every_preset_name_exists_in_the_pinned_packs() {
        // Preset names copied from Github833's rules.txt files (and the embedded FPS++): a typo would make Cemu fall back
        // to the default silently, so pin them here.
        let known = [
            "16:9 (Default)", "16:10", "21:9", "3840x2160 (4K)", "2560x1440 (2K)", "1920x1080 (Full HD)", "1280x720 (HD, Default)",
            "3840x2400", "2560x1600", "1920x1200", "3840x1600", "2560x1080", "Normal FXAA (Default)", "Ultra (300%)", "High (200%)",
            "Medium (100%, Default)", "Extreme (16x)", "Ultra (8x, Default)", "Enhanced Reflections", "Normal Reflections",
            "Ultra (1.5x)", "Ultra (1.5x, requires Extended Memory pack!)", "High (1.25x)", "Medium (1x)", "Higher (-2)",
            "Slightly higher (-1)", "Normal (Default)", "Normal Settings", "60FPS (ideal for 240/120/60Hz displays)",
            "120FPS (ideal for 240/120/60Hz displays)", "144FPS (ideal for 144Hz displays)",
        ];
        for m in [main_pc(), strix()] {
            for q in [Quality::Max, Quality::High, Quality::Balanced] {
                for fps in [FpsCap::Auto, FpsCap::Fps60, FpsCap::Fps120, FpsCap::Fps144] {
                    for (_, presets) in plan(&m, GraphicsChoice { quality: q, fps }).1 {
                        for (_, p) in presets {
                            assert!(known.contains(&p), "unknown preset {p:?}");
                        }
                    }
                }
            }
        }
    }

    /// On a real PC: `cargo test -p privcord-games --lib real_machine -- --ignored --nocapture` prints the detected
    /// machine and the settings.xml Cemu 1.26.2 would get (from a copy; nothing is written).
    #[test]
    #[ignore]
    fn real_machine() {
        let m = detect_machine().expect("no display (run from the desktop session)");
        println!("{m:?}");
        let p = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").unwrap())
            .join("com.privcord.app/native-games/botwmp/app/cemu_1.26.2/settings.xml");
        let xml = std::fs::read_to_string(p).unwrap();
        let (out, q) = apply_to_xml(&xml, &m, GraphicsChoice::default(), true);
        println!("quality {q:?}
{}", &out[out.find("<GraphicPack>").unwrap()..out.find("</Graphic>").unwrap()]);
        assert!(out.contains("<api>1</api>") && out.contains("<AsyncCompile>true</AsyncCompile>"));
    }

    #[test]
    fn upserts_entries_and_values() {
        let xml = "<?xml version=\"1.0\"?>\n<content>\n    <GraphicPack>\n        <Entry filename=\"graphicPacks\\BreathOfTheWild_BCML\\rules.txt\"/>\n        <Entry filename=\"graphicPacks\\downloadedGraphicPacks\\BreathOfTheWild\\Mods\\FPS++\\rules.txt\"/>\n    </GraphicPack>\n    <Graphic>\n        <api>0</api>\n    </Graphic>\n</content>\n";
        let a = upsert_entry(xml, FPS_PLUS_PLUS, &[("Mode", "Normal Settings"), ("Framerate Limit", "120FPS (ideal for 240/120/60Hz displays)")]);
        assert_eq!(a.matches("FPS++").count(), 1, "replaced, not duplicated:\n{a}");
        assert!(a.contains("<category>Framerate Limit</category>\n                <preset>120FPS (ideal for 240/120/60Hz displays)</preset>"));
        assert!(a.contains("BreathOfTheWild_BCML"));
        // Idempotent, and replaces an entry that already has presets.
        let b = upsert_entry(&a, FPS_PLUS_PLUS, &[("Mode", "Normal Settings"), ("Framerate Limit", "120FPS (ideal for 240/120/60Hz displays)")]);
        assert_eq!(a, b);
        let c = upsert_entry(&b, GRAPHICS, &[("Shadows", "Ultra (300%)")]);
        assert!(c.contains("<preset>Ultra (300%)</preset>") && c.find("Graphics\\rules").unwrap() < c.find("</GraphicPack>").unwrap());
        let d = set_value(&c, "Graphic", "api", "1");
        assert!(d.contains("<api>1</api>"));
        let e = set_value(&d, "Graphic", "AsyncCompile", "true");
        assert!(e.contains("        <AsyncCompile>true</AsyncCompile>\n    </Graphic>"));
    }
}
