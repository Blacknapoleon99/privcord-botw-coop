//! BotW co-op difficulty: a Cemu graphic pack that gives enemies and bosses more health, generated on the player's PC
//! from their own game dump (PrivCord never ships Nintendo files).
//!
//! Every enemy actor pack (`Actor/Pack/<Name>.sbactorpack`, a Yaz0-compressed SARC) holds a `GeneralParamList`
//! (`.bgparamlist`, AAMP) whose `General/Life` int is the enemy's health; `Actor/ActorInfo.product.sbyml` (BYML) caches the
//! same number as `generalLife`. Both are edited **in place**: only the 4 bytes of the int change, so every file keeps its
//! decompressed size and the resource size table (RSTB) of the BCML-merged pack stays valid without being touched.
//!
//! Sources, in priority order: the BCML-merged pack (`graphicPacks/BreathOfTheWild_BCML/content`, which the BOTWM mod
//! needs and which already overrides some actor packs and ActorInfo), then the v208 update's `content`. The output is
//! its own graphic pack with a higher `fsPriority` than BCML's 9999, so BCML's pack is never modified.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};

/// Folder name of the generated pack under `graphicPacks`.
pub const PACK_DIR: &str = "PrivCordCoopDifficulty";
/// Allowed enemy health settings, in percent of vanilla. 100 = off (no pack).
pub const CHOICES: [u16; 5] = [100, 150, 200, 250, 300];
/// Written next to `rules.txt`; regeneration is skipped when it already matches.
const STAMP_FILE: &str = "privcord-stamp.txt";
/// `fsPriority` of the BCML pack is 9999; ours must be loaded over it.
const FS_PRIORITY: u32 = 10000;

/// Actor profiles that are real fights. Dragons, divine-beast parts (`Remains*`), bullets, weapons and animals are left
/// alone.
const PROFILES: [&str; 7] = ["Enemy", "GiantEnemy", "Guardian", "Sandworm", "LastBoss", "SiteBoss", "GelEnemy"];

/// Whether an actor's health is scaled. `life` is its vanilla health.
pub fn should_scale(name: &str, profile: &str, life: i32) -> bool {
    let fighter = name.starts_with("Enemy_") || name.starts_with("Priest_Boss_");
    // Weak points and Dark Beast Ganon are scripted (one hit / light arrows), and 1-HP actors are one-hit by design
    // (Malice eyes, Stal heads, shadow clones).
    let scripted = name.contains("WeakPoint") || name.starts_with("Enemy_GanonBeast");
    fighter && !scripted && life > 1 && PROFILES.contains(&profile)
}

/// Scaled health, rounded to the nearest point.
pub fn scaled(life: i32, percent: u16) -> i32 {
    let v = (i64::from(life) * i64::from(percent) + 50) / 100;
    v.clamp(1, i64::from(i32::MAX)) as i32
}

/// Clamp a requested percentage to the nearest allowed choice.
pub fn normalize_percent(percent: u16) -> u16 {
    *CHOICES.iter().min_by_key(|c| (i32::from(**c) - i32::from(percent)).abs()).unwrap()
}

// ---------------------------------------------------------------------------------------------------------------------
// Formats

/// CRC-32 (IEEE), used by AAMP for parameter names.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

pub mod yaz0 {
    use anyhow::{Result, bail, ensure};

    pub fn is_yaz0(data: &[u8]) -> bool {
        data.len() >= 16 && &data[..4] == b"Yaz0"
    }

    pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
        ensure!(is_yaz0(src), "not Yaz0");
        let size = u32::from_be_bytes(src[4..8].try_into().unwrap()) as usize;
        let mut out = Vec::with_capacity(size);
        let mut i = 16;
        while out.len() < size {
            let Some(&group) = src.get(i) else { bail!("Yaz0: truncated") };
            i += 1;
            for bit in (0..8).rev() {
                if out.len() >= size {
                    break;
                }
                if group & (1 << bit) != 0 {
                    let Some(&b) = src.get(i) else { bail!("Yaz0: truncated") };
                    out.push(b);
                    i += 1;
                } else {
                    let (Some(&b1), Some(&b2)) = (src.get(i), src.get(i + 1)) else { bail!("Yaz0: truncated") };
                    i += 2;
                    let dist = ((usize::from(b1 & 0x0F) << 8) | usize::from(b2)) + 1;
                    let len = if b1 >> 4 == 0 {
                        let Some(&b3) = src.get(i) else { bail!("Yaz0: truncated") };
                        i += 1;
                        usize::from(b3) + 0x12
                    } else {
                        usize::from(b1 >> 4) + 2
                    };
                    ensure!(dist <= out.len(), "Yaz0: bad back-reference");
                    let start = out.len() - dist;
                    for k in 0..len {
                        let b = out[start + k];
                        out.push(b);
                    }
                }
            }
        }
        out.truncate(size);
        Ok(out)
    }

    const WINDOW: usize = 0x1000;
    const MAX_LEN: usize = 0x111;
    const CHAIN: usize = 96;

    /// Greedy LZ77 with hash chains. Not as tight as Nintendo's encoder, but valid Yaz0 and fast.
    pub fn compress(src: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(src.len() / 2 + 32);
        out.extend_from_slice(b"Yaz0");
        out.extend_from_slice(&(src.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0; 8]);
        const HASH: usize = 1 << 15;
        let mut head = vec![usize::MAX; HASH];
        let mut prev = vec![usize::MAX; src.len()];
        let hash = |p: usize| -> usize {
            ((usize::from(src[p]) << 10) ^ (usize::from(src[p + 1]) << 5) ^ usize::from(src[p + 2])) & (HASH - 1)
        };
        let insert = |p: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
            if p + 3 <= src.len() {
                let h = hash(p);
                prev[p] = head[h];
                head[h] = p;
            }
        };
        let mut pos = 0;
        while pos < src.len() {
            let group_at = out.len();
            out.push(0);
            let mut group = 0u8;
            for bit in (0..8).rev() {
                if pos >= src.len() {
                    break;
                }
                let (mut best_len, mut best_pos) = (0, 0);
                if pos + 3 <= src.len() {
                    let max = MAX_LEN.min(src.len() - pos);
                    let mut cand = head[hash(pos)];
                    let mut steps = 0;
                    while cand != usize::MAX && pos - cand <= WINDOW && steps < CHAIN {
                        let mut l = 0;
                        while l < max && src[cand + l] == src[pos + l] {
                            l += 1;
                        }
                        if l > best_len {
                            best_len = l;
                            best_pos = cand;
                            if l == max {
                                break;
                            }
                        }
                        cand = prev[cand];
                        steps += 1;
                    }
                }
                if best_len >= 3 {
                    let dist = pos - best_pos - 1;
                    if best_len >= 0x12 {
                        out.push((dist >> 8) as u8);
                        out.push(dist as u8);
                        out.push((best_len - 0x12) as u8);
                    } else {
                        out.push((((best_len - 2) << 4) | (dist >> 8)) as u8);
                        out.push(dist as u8);
                    }
                    for p in pos..pos + best_len {
                        insert(p, &mut head, &mut prev);
                    }
                    pos += best_len;
                } else {
                    group |= 1 << bit;
                    out.push(src[pos]);
                    insert(pos, &mut head, &mut prev);
                    pos += 1;
                }
            }
            out[group_at] = group;
        }
        out
    }
}

#[derive(Clone, Copy)]
enum Endian {
    Big,
    Little,
}

impl Endian {
    fn u16(self, d: &[u8], o: usize) -> Result<u16> {
        let b: [u8; 2] = d.get(o..o + 2).ok_or_else(|| anyhow!("truncated at {o:#x}"))?.try_into()?;
        Ok(match self {
            Endian::Big => u16::from_be_bytes(b),
            Endian::Little => u16::from_le_bytes(b),
        })
    }
    fn u24(self, d: &[u8], o: usize) -> Result<u32> {
        let b = d.get(o..o + 3).ok_or_else(|| anyhow!("truncated at {o:#x}"))?;
        Ok(match self {
            Endian::Big => u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]),
            Endian::Little => u32::from(b[2]) << 16 | u32::from(b[1]) << 8 | u32::from(b[0]),
        })
    }
    fn u32(self, d: &[u8], o: usize) -> Result<u32> {
        let b: [u8; 4] = d.get(o..o + 4).ok_or_else(|| anyhow!("truncated at {o:#x}"))?.try_into()?;
        Ok(match self {
            Endian::Big => u32::from_be_bytes(b),
            Endian::Little => u32::from_le_bytes(b),
        })
    }
    fn i32(self, d: &[u8], o: usize) -> Result<i32> {
        Ok(self.u32(d, o)? as i32)
    }
    fn put_i32(self, d: &mut [u8], o: usize, v: i32) {
        let b = match self {
            Endian::Big => v.to_be_bytes(),
            Endian::Little => v.to_le_bytes(),
        };
        d[o..o + 4].copy_from_slice(&b);
    }
}

fn c_str(d: &[u8], o: usize) -> Result<&str> {
    let tail = d.get(o..).ok_or_else(|| anyhow!("string out of range"))?;
    let end = tail.iter().position(|&b| b == 0).ok_or_else(|| anyhow!("unterminated string"))?;
    Ok(std::str::from_utf8(&tail[..end])?)
}

/// Files in a SARC archive: `(name, start, end)` as absolute offsets into `d`.
pub fn sarc_files(d: &[u8]) -> Result<Vec<(String, usize, usize)>> {
    ensure!(d.len() >= 0x14 && &d[..4] == b"SARC", "not a SARC");
    let e = if d[6..8] == [0xFE, 0xFF] { Endian::Big } else { Endian::Little };
    let header = usize::from(e.u16(d, 4)?);
    let data_start = e.u32(d, 0x0C)? as usize;
    let sfat = header;
    ensure!(d.get(sfat..sfat + 4) == Some(b"SFAT"), "SARC: no SFAT");
    let nodes = sfat + usize::from(e.u16(d, sfat + 4)?);
    let count = usize::from(e.u16(d, sfat + 6)?);
    let sfnt = nodes + count * 16;
    ensure!(d.get(sfnt..sfnt + 4) == Some(b"SFNT"), "SARC: no SFNT");
    let names = sfnt + usize::from(e.u16(d, sfnt + 4)?);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let n = nodes + i * 16;
        let attr = e.u32(d, n + 4)?;
        let (s, t) = (data_start + e.u32(d, n + 8)? as usize, data_start + e.u32(d, n + 12)? as usize);
        ensure!(s <= t && t <= d.len(), "SARC: file out of range");
        let name = if attr & 0x0100_0000 != 0 { c_str(d, names + (attr & 0xFFFF) as usize * 4)?.to_string() } else { String::new() };
        out.push((name, s, t));
    }
    Ok(out)
}

/// Offset (in `d`) and byte order of the Int parameter `object/param` anywhere in an AAMP parameter IO.
fn aamp_find_int(d: &[u8], object: &str, param: &str) -> Result<Option<(usize, Endian)>> {
    ensure!(d.len() >= 0x30 && &d[..4] == b"AAMP", "not AAMP");
    let e = if Endian::Little.u32(d, 8)? & 1 != 0 { Endian::Little } else { Endian::Big };
    let root = 0x30 + e.u32(d, 0x14)? as usize;
    let (oh, ph) = (crc32(object.as_bytes()), crc32(param.as_bytes()));
    let mut lists = vec![root];
    let mut guard = 0;
    while let Some(lo) = lists.pop() {
        guard += 1;
        ensure!(guard < 10_000, "AAMP: too many lists");
        let (l_rel, l_n) = (usize::from(e.u16(d, lo + 4)?), usize::from(e.u16(d, lo + 6)?));
        let (o_rel, o_n) = (usize::from(e.u16(d, lo + 8)?), usize::from(e.u16(d, lo + 10)?));
        lists.extend((0..l_n).map(|i| lo + l_rel * 4 + i * 12));
        for i in 0..o_n {
            let oo = lo + o_rel * 4 + i * 8;
            if e.u32(d, oo)? != oh {
                continue;
            }
            let (p_rel, p_n) = (usize::from(e.u16(d, oo + 4)?), usize::from(e.u16(d, oo + 6)?));
            for j in 0..p_n {
                let po = oo + p_rel * 4 + j * 8;
                if e.u32(d, po)? != ph {
                    continue;
                }
                let packed = e.u32(d, po + 4)?;
                const INT: u32 = 2;
                ensure!(packed >> 24 == INT, "AAMP: {object}/{param} is not an int");
                let at = po + (packed & 0x00FF_FFFF) as usize * 4;
                ensure!(at + 4 <= d.len(), "AAMP: value out of range");
                return Ok(Some((at, e)));
            }
        }
    }
    Ok(None)
}

/// One ActorInfo entry: name, profile and where its `generalLife` int lives (if it has one).
pub struct ActorInfoEntry {
    pub name: String,
    pub profile: String,
    pub life: Option<i32>,
    life_at: Option<usize>,
}

/// Read the `Actors` array of a decompressed `ActorInfo.product.byml`.
fn actor_info(d: &[u8]) -> Result<(Endian, Vec<ActorInfoEntry>)> {
    let e = match d.get(..2) {
        Some(b"BY") => Endian::Big,
        Some(b"YB") => Endian::Little,
        _ => bail!("not BYML"),
    };
    let string_table = |o: usize| -> Result<Vec<String>> {
        ensure!(d.get(o) == Some(&0xC2), "BYML: bad string table");
        let n = e.u24(d, o + 1)? as usize;
        (0..n).map(|i| Ok(c_str(d, o + e.u32(d, o + 4 + 4 * i)? as usize)?.to_string())).collect()
    };
    let keys = string_table(e.u32(d, 4)? as usize)?;
    let strings = string_table(e.u32(d, 8)? as usize)?;
    // (key, node type, value, offset of the value)
    let dict = |o: usize| -> Result<Vec<(&str, u8, u32, usize)>> {
        ensure!(d.get(o) == Some(&0xC1), "BYML: expected a dictionary");
        let n = e.u24(d, o + 1)? as usize;
        (0..n)
            .map(|i| {
                let eo = o + 4 + i * 8;
                let k = keys.get(e.u24(d, eo)? as usize).ok_or_else(|| anyhow!("BYML: bad key"))?;
                Ok((k.as_str(), d[eo + 3], e.u32(d, eo + 4)?, eo + 4))
            })
            .collect()
    };
    let root = dict(e.u32(d, 12)? as usize)?;
    let &(_, _, actors, _) = root.iter().find(|(k, ..)| *k == "Actors").ok_or_else(|| anyhow!("ActorInfo: no Actors"))?;
    let actors = actors as usize;
    ensure!(d.get(actors) == Some(&0xC0), "ActorInfo: Actors is not an array");
    let n = e.u24(d, actors + 1)? as usize;
    let values = actors + 4 + ((n + 3) & !3);
    let string = |v: u32| strings.get(v as usize).cloned().ok_or_else(|| anyhow!("BYML: bad string index"));
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let entry = dict(e.u32(d, values + 4 * i)? as usize)?;
        let (mut name, mut profile, mut life, mut life_at) = (String::new(), String::new(), None, None);
        for (k, t, v, at) in entry {
            match (k, t) {
                ("name", 0xA0) => name = string(v)?,
                ("profile", 0xA0) => profile = string(v)?,
                ("generalLife", 0xD1) => {
                    life = Some(v as i32);
                    life_at = Some(at);
                }
                _ => {}
            }
        }
        out.push(ActorInfoEntry { name, profile, life, life_at });
    }
    Ok((e, out))
}

/// Scale `General/Life` inside a decompressed actor pack. Returns `Some((old, new))` if a GeneralParamList was found.
pub fn scale_actor_pack(pack: &mut [u8], percent: u16) -> Result<Option<(i32, i32)>> {
    let files = sarc_files(pack)?;
    let Some((_, s, t)) = files.into_iter().find(|(n, ..)| n.starts_with("Actor/GeneralParamList/") && n.ends_with(".bgparamlist")) else {
        return Ok(None);
    };
    let Some((at, e)) = aamp_find_int(&pack[s..t], "General", "Life")? else { return Ok(None) };
    let old = e.i32(&pack[s..t], at)?;
    let new = scaled(old, percent);
    e.put_i32(&mut pack[s..t], at, new);
    Ok(Some((old, new)))
}

// ---------------------------------------------------------------------------------------------------------------------
// Pack generation

/// Where the game files come from.
pub struct Sources {
    /// `.../Update v208/content`.
    pub update_content: PathBuf,
    /// `graphicPacks/BreathOfTheWild_BCML/content`, if BCML merged mods (BOTWM does).
    pub bcml_content: Option<PathBuf>,
}

impl Sources {
    /// The file to start from: BCML's merged copy if it has one, else the update's.
    fn pick(&self, rel: &str) -> Option<PathBuf> {
        self.bcml_content.iter().chain(std::iter::once(&self.update_content)).map(|b| b.join(rel)).find(|p| p.is_file())
    }
}

#[derive(Debug, Default)]
pub struct Report {
    /// Actor packs written.
    pub packs: usize,
    /// ActorInfo entries changed.
    pub actor_info: usize,
    /// (actor, old life, new life), sorted by name.
    pub changes: Vec<(String, i32, i32)>,
}

fn rules_txt(percent: u16) -> String {
    format!(
        "[Definition]\r\ntitleIds = 00050000101C9300,00050000101C9400,00050000101C9500\r\nname = PrivCord co-op difficulty ({percent}% enemy health, Made by Blackz)\r\n\
         path = \"The Legend of Zelda: Breath of the Wild/Mods/PrivCord co-op difficulty\"\r\n\
         description = Enemies and bosses have {percent}% health. Generated by PrivCord from this PC's own game files. BOTW Multiplayer by its original team; PrivCord integration and co-op features by Blackz.\r\n\
         version = 6\r\nfsPriority = {FS_PRIORITY}\r\n"
    )
}

/// A cheap fingerprint of the inputs (sizes and modification times of ActorInfo in each source), so a BCML re-merge or a
/// game update forces regeneration.
fn stamp(sources: &Sources, percent: u16) -> String {
    let mut s = format!("percent={percent};v=1");
    for base in sources.bcml_content.iter().chain(std::iter::once(&sources.update_content)) {
        let p = base.join("Actor/ActorInfo.product.sbyml");
        if let Ok(m) = fs::metadata(&p) {
            let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            s.push_str(&format!(";{}={}@{}", p.display(), m.len(), t));
        }
    }
    s
}

/// True if `out` already holds the pack for `percent` built from the same inputs.
pub fn is_current(sources: &Sources, out: &Path, percent: u16) -> bool {
    fs::read_to_string(out.join(STAMP_FILE)).is_ok_and(|s| s == stamp(sources, percent)) && out.join("rules.txt").is_file()
}

/// Build the pack into `out` (replacing whatever was there). `percent` must be > 100; for 100 use [`remove`].
pub fn generate(sources: &Sources, out: &Path, percent: u16) -> Result<Report> {
    ensure!(percent > 100 && percent <= 1000, "enemy health must be 101–1000%, got {percent}");
    let info_rel = "Actor/ActorInfo.product.sbyml";
    let info_path = sources.pick(info_rel).ok_or_else(|| anyhow!("ActorInfo.product.sbyml not found in the game files"))?;
    let mut info = yaz0::decompress(&fs::read(&info_path).with_context(|| format!("read {}", info_path.display()))?)?;
    let (e, actors) = actor_info(&info)?;

    // Build in a sibling temp folder, then swap, so a failure never leaves a half-written pack enabled.
    let tmp = out.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("content/Actor/Pack"))?;

    let mut report = Report::default();
    for a in &actors {
        let (Some(life), Some(at)) = (a.life, a.life_at) else { continue };
        if !should_scale(&a.name, &a.profile, life) {
            continue;
        }
        e.put_i32(&mut info, at, scaled(life, percent));
        report.actor_info += 1;

        let rel = format!("Actor/Pack/{}.sbactorpack", a.name);
        let Some(src) = sources.pick(&rel) else { continue };
        let raw = fs::read(&src).with_context(|| format!("read {}", src.display()))?;
        let mut pack = if yaz0::is_yaz0(&raw) { yaz0::decompress(&raw)? } else { raw };
        match scale_actor_pack(&mut pack, percent).with_context(|| format!("patch {}", a.name))? {
            Some((old, new)) => {
                fs::write(tmp.join("content").join(&rel), yaz0::compress(&pack))?;
                report.packs += 1;
                report.changes.push((a.name.clone(), old, new));
            }
            None => tracing::debug!(actor = %a.name, "no General/Life in actor pack"),
        }
    }
    ensure!(report.packs > 0, "no enemy actor packs found under {}", sources.update_content.display());
    fs::write(tmp.join("content").join(info_rel), yaz0::compress(&info))?;
    fs::write(tmp.join("rules.txt"), rules_txt(percent))?;
    fs::write(tmp.join(STAMP_FILE), stamp(sources, percent))?;

    remove(out)?;
    fs::rename(&tmp, out).with_context(|| format!("move pack into {}", out.display()))?;
    report.changes.sort();
    Ok(report)
}

/// Delete the generated pack (difficulty back to vanilla).
pub fn remove(out: &Path) -> Result<()> {
    match fs::remove_dir_all(out) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", out.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_values() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        // The DLL's PorchItem scan signature (PLAN.md §1.1).
        assert_eq!(crc32(b"PorchItem"), 0x5f28_3289);
    }

    #[test]
    fn yaz0_round_trips() {
        let mut data = Vec::new();
        for i in 0..20_000u32 {
            data.extend_from_slice(&(i % 97).to_be_bytes());
            if i % 13 == 0 {
                data.extend_from_slice(b"Enemy_Bokoblin_Junior");
            }
        }
        data.extend(std::iter::repeat_n(0xAB, 1000)); // longer than one 0x111 run
        for input in [&b""[..], b"a", b"abcabcabcabc", &data] {
            let c = yaz0::compress(input);
            assert_eq!(yaz0::decompress(&c).unwrap(), input);
        }
        assert!(yaz0::compress(&data).len() < data.len() / 2);
    }

    #[test]
    fn scaling_policy() {
        assert!(should_scale("Enemy_Lynel_Gold", "Enemy", 7500));
        assert!(should_scale("Enemy_Ganon", "LastBoss", 8000));
        assert!(should_scale("Priest_Boss_Giant", "Enemy", 2000));
        assert!(!should_scale("Enemy_Golem_WeakPoint", "Enemy", 100));
        assert!(!should_scale("Enemy_GanonBeast", "Enemy", 8));
        assert!(!should_scale("Enemy_Keese_Swarm", "EnemySwarm", 1));
        assert!(!should_scale("Enemy_Dragon_Fire", "Dragon", 100));
        assert!(!should_scale("RemainsElectricCannon", "Enemy", 2000));
        assert!(!should_scale("Enemy_Moriblin_Bone_Junior_Head", "Enemy", 1));
        assert_eq!(scaled(13, 150), 20);
        assert_eq!(scaled(8000, 200), 16000);
        assert_eq!(normalize_percent(170), 150);
        assert_eq!(normalize_percent(9), 100);
    }

    /// A minimal big-endian SARC holding one AAMP `.bgparamlist` with `General/Life = 72`, laid out like the game's.
    fn synthetic_pack(life: i32) -> Vec<u8> {
        // AAMP (little-endian flag set, as BotW's are): root list with one object "General" holding "Life" (int).
        let mut a = vec![0u8; 0x30];
        a[..4].copy_from_slice(b"AAMP");
        a[4..8].copy_from_slice(&2u32.to_le_bytes());
        a[8..12].copy_from_slice(&3u32.to_le_bytes()); // LE + UTF-8
        a[0x14..0x18].copy_from_slice(&4u32.to_le_bytes()); // pio offset: 4-byte type string
        a.extend_from_slice(b"xml\0");
        let root = a.len();
        // list: name, lists rel/count, objects rel/count (rel in words from the list)
        a.extend_from_slice(&crc32(b"param_root").to_le_bytes());
        a.extend_from_slice(&3u16.to_le_bytes());
        a.extend_from_slice(&0u16.to_le_bytes());
        a.extend_from_slice(&3u16.to_le_bytes()); // object at root + 12
        a.extend_from_slice(&1u16.to_le_bytes());
        let obj = a.len();
        assert_eq!(obj, root + 12);
        a.extend_from_slice(&crc32(b"General").to_le_bytes());
        a.extend_from_slice(&2u16.to_le_bytes()); // params at obj + 8
        a.extend_from_slice(&2u16.to_le_bytes());
        // two params: "Speed" (int) then "Life" (int), data after both
        let p0 = a.len();
        let data = p0 + 16;
        for (i, name) in [&b"Speed"[..], b"Life"].iter().enumerate() {
            let po = p0 + i * 8;
            let rel = ((data + i * 4 - po) / 4) as u32;
            a.extend_from_slice(&crc32(name).to_le_bytes());
            a.extend_from_slice(&(rel | 2 << 24).to_le_bytes());
        }
        a.extend_from_slice(&5i32.to_le_bytes());
        a.extend_from_slice(&life.to_le_bytes());

        let name = b"Actor/GeneralParamList/Test.bgparamlist\0\0\0\0";
        let names_len = (name.len() + 3) & !3;
        let header = 0x14;
        let sfat = header;
        let nodes = sfat + 0x0C;
        let sfnt = nodes + 16;
        let names = sfnt + 8;
        let data_start = (names + names_len + 0x7F) & !0x7F;
        let mut s = vec![0u8; data_start];
        s[..4].copy_from_slice(b"SARC");
        s[4..6].copy_from_slice(&0x14u16.to_be_bytes());
        s[6..8].copy_from_slice(&[0xFE, 0xFF]);
        s[0x0C..0x10].copy_from_slice(&(data_start as u32).to_be_bytes());
        s[sfat..sfat + 4].copy_from_slice(b"SFAT");
        s[sfat + 4..sfat + 6].copy_from_slice(&0x0Cu16.to_be_bytes());
        s[sfat + 6..sfat + 8].copy_from_slice(&1u16.to_be_bytes());
        s[nodes + 4..nodes + 8].copy_from_slice(&0x0100_0000u32.to_be_bytes());
        s[nodes + 8..nodes + 12].copy_from_slice(&0u32.to_be_bytes());
        s[nodes + 12..nodes + 16].copy_from_slice(&(a.len() as u32).to_be_bytes());
        s[sfnt..sfnt + 4].copy_from_slice(b"SFNT");
        s[sfnt + 4..sfnt + 6].copy_from_slice(&8u16.to_be_bytes());
        s[names..names + name.len()].copy_from_slice(name);
        s.extend_from_slice(&a);
        let len = s.len() as u32;
        s[8..12].copy_from_slice(&len.to_be_bytes());
        s
    }

    #[test]
    fn scales_life_in_place_keeping_size() {
        let mut pack = synthetic_pack(72);
        let before = pack.clone();
        assert_eq!(scale_actor_pack(&mut pack, 200).unwrap(), Some((72, 144)));
        assert_eq!(pack.len(), before.len());
        let diff: Vec<usize> = (0..pack.len()).filter(|&i| pack[i] != before[i]).collect();
        assert!(diff.len() <= 4 && diff.iter().all(|&i| i >= pack.len() - 4), "only the Life int changes: {diff:?}");
        // The neighbouring int is untouched.
        assert_eq!(&pack[pack.len() - 8..pack.len() - 4], &5i32.to_le_bytes());
    }

    /// Runs against a real dump: `BOTW_UPDATE_CONTENT=<...Update v208\content> [BOTW_BCML_CONTENT=...]
    /// cargo test -p privcord-games --lib real_dump -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_dump() {
        let update = std::env::var_os("BOTW_UPDATE_CONTENT").expect("set BOTW_UPDATE_CONTENT");
        let sources = Sources { update_content: update.into(), bcml_content: std::env::var_os("BOTW_BCML_CONTENT").map(Into::into) };
        let out = std::env::temp_dir().join("privcord-coop-difficulty-test");
        let t = std::time::Instant::now();
        let r = generate(&sources, &out, 200).unwrap();
        println!("{} packs, {} ActorInfo entries in {:?}", r.packs, r.actor_info, t.elapsed());
        for (n, a, b) in &r.changes {
            println!("{n}: {a} -> {b}");
        }
        assert!(r.packs > 120 && r.packs == r.changes.len());
        assert!(is_current(&sources, &out, 200));
        assert!(!is_current(&sources, &out, 150));
        let lynel = r.changes.iter().find(|c| c.0 == "Enemy_Lynel_Gold").unwrap();
        assert_eq!((lynel.1, lynel.2), (7500, 15000));
    }
}
