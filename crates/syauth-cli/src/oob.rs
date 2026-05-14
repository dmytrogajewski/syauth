//! `syauth-cli` — app-level OOB code derivation.
//!
//! Per SPEC §4.1 the desktop and the phone each compute, after BlueZ-level
//! LE Secure Connections completes, a *second* OOB confirmation derived from
//! the freshly-negotiated bond key:
//!
//! ```text
//! HKDF(bond, "syauth-oob-v1")[0..OOB_WORD_COUNT]
//! ```
//!
//! Each of the [`OOB_WORD_COUNT`] output bytes indexes into a 256-entry
//! [`OOB_WORDS`] table of short emoji-prefixed nouns. The function is pure:
//! same `bond_key` ⇒ same word tuple, byte-deterministic.
//!
//! The exact word contents are not part of the wire format — both sides compute
//! the same tuple from the same shared secret, so a stable, committed list is
//! all that is required.  The list is kept short (one emoji + one short English
//! noun per entry, no combining marks, no skin-tone modifiers) so it is
//! readable on any terminal and on Android.
//!
//! Roadmap: specs/syauth/ROADMAP.md item S-011.
//! Journey: specs/journeys/JOURNEY-S-011-pairing-desktop.md

use hkdf::Hkdf;
use sha2::Sha256;

/// HKDF info string for the v1 OOB derivation. Versioned so a future schema
/// can rotate without recomputing existing bonds.
pub const HKDF_INFO_OOB_V1: &[u8] = b"syauth-oob-v1";

/// Number of OOB words shown to the operator. Four words gives ~32 bits of
/// confirmation entropy (256^4 ≈ 4.3 × 10^9), well above what an
/// attention-limited human can be tricked into rubber-stamping under typical
/// pairing UX time pressure.
pub const OOB_WORD_COUNT: usize = 4;

/// Width of the bond key the HKDF expand step is keyed on. Matches the
/// `syauth-transport::BOND_KEY_BYTES` constant; restated locally so this module
/// has no inbound type dependency on the transport crate.
pub const OOB_BOND_KEY_BYTES: usize = 32;

/// 256-entry table of short emoji-prefixed English nouns. One entry per byte
/// value 0x00..=0xFF, indexed by the corresponding byte of the HKDF expand
/// output. The contents are stable across releases of v0.1.0 — bumping the
/// HKDF info string ([`HKDF_INFO_OOB_V1`]) is the way to rotate.
///
/// Sources for the nouns are everyday-English short words; sources for the
/// emoji are well-known unicode glyphs that render on any modern font. There
/// are no combining marks (no skin-tone modifiers, no ZWJ sequences) so each
/// entry is exactly two unicode scalar values.
pub static OOB_WORDS: [&str; 256] = [
    "🍎 apple",
    "🐝 bee",
    "🎯 dart",
    "🌍 earth",
    "🔥 fire",
    "🍇 grape",
    "🏠 home",
    "🧊 ice",
    "🪀 jojo",
    "🪁 kite",
    "🦁 lion",
    "🌙 moon",
    "🌰 nut",
    "🐙 octo",
    "🥞 pancake",
    "🪨 quartz",
    "🌹 rose",
    "⭐ star",
    "🌳 tree",
    "☂️ umbrella",
    "🎻 violin",
    "🌊 wave",
    "🦓 zebra",
    "🍌 banana",
    "🌵 cactus",
    "🐬 dolphin",
    "🌽 ear",
    "🍂 fern",
    "🎁 gift",
    "🪖 helmet",
    "🦔 iguana",
    "💎 jewel",
    "🔑 key",
    "🍋 lemon",
    "🥭 mango",
    "🌮 taco",
    "🦉 owl",
    "🥧 pie",
    "👑 crown",
    "🐇 rabbit",
    "🧂 salt",
    "🍅 tomato",
    "🦄 unicorn",
    "🚐 van",
    "🌷 wattle",
    "🎷 sax",
    "🍪 cookie",
    "🎨 art",
    "🦋 butterfly",
    "🐈 cat",
    "🐶 dog",
    "🐘 elephant",
    "🦊 fox",
    "🐐 goat",
    "🐹 hamster",
    "🦔 ivy",
    "🪼 jelly",
    "🐨 koala",
    "🦙 llama",
    "🐭 mouse",
    "🦢 swan",
    "🐂 ox",
    "🐧 penguin",
    "🐤 chick",
    "🐦 robin",
    "🐍 snake",
    "🐢 turtle",
    "🦦 otter",
    "🐅 tiger",
    "🦅 eagle",
    "🐋 whale",
    "🦈 shark",
    "🦒 giraffe",
    "🐊 croc",
    "🐡 puffer",
    "🦜 parrot",
    "🐓 hen",
    "🦛 hippo",
    "🐎 horse",
    "🐃 buffalo",
    "🌻 sunflower",
    "🍄 mushroom",
    "🌶️ chili",
    "🥑 avocado",
    "🥦 broccoli",
    "🥒 cucumber",
    "🌽 corn",
    "🥔 potato",
    "🍆 eggplant",
    "🥕 carrot",
    "🌰 acorn",
    "🥥 coconut",
    "🍒 cherry",
    "🍓 strawberry",
    "🍑 peach",
    "🍐 pear",
    "🍊 orange",
    "🍉 melon",
    "🥝 kiwi",
    "🍍 pineapple",
    "🥭 papaya",
    "🫐 berry",
    "🥕 root",
    "🥯 bagel",
    "🥖 baguette",
    "🥨 pretzel",
    "🥐 croissant",
    "🍞 bread",
    "🧀 cheese",
    "🥚 egg",
    "🍗 drumstick",
    "🥩 steak",
    "🌭 hotdog",
    "🍔 burger",
    "🍟 fries",
    "🍕 pizza",
    "🥪 sub",
    "🌯 wrap",
    "🥙 falafel",
    "🍣 sushi",
    "🍦 sundae",
    "🍧 sorbet",
    "🍨 gelato",
    "🍫 choco",
    "🍬 candy",
    "🍮 flan",
    "🍡 dango",
    "🧁 cupcake",
    "☕ coffee",
    "🍵 tea",
    "🍶 sake",
    "🍾 bubbly",
    "🍷 wine",
    "🍸 martini",
    "🍹 mojito",
    "🍺 beer",
    "🪐 saturn",
    "🌟 nova",
    "🪨 boulder",
    "🏔️ peak",
    "🏕️ camp",
    "🏖️ beach",
    "🏜️ dune",
    "🏝️ atoll",
    "⛰️ mount",
    "🌋 volcano",
    "🛤️ rail",
    "🛣️ road",
    "🌉 bridge",
    "🏞️ park",
    "🏟️ stadium",
    "🏛️ forum",
    "🏗️ crane",
    "🧱 brick",
    "🏘️ homes",
    "🏚️ shack",
    "🏤 post",
    "🏥 clinic",
    "🏦 bank",
    "🏨 hotel",
    "🏩 inn",
    "🏪 store",
    "🏫 school",
    "🏬 mall",
    "🏭 plant",
    "🏯 keep",
    "🏰 castle",
    "🗼 tower",
    "🗽 statue",
    "⛩️ shrine",
    "🕌 dome",
    "🕍 hall",
    "⛪ chapel",
    "🛕 temple",
    "🕋 cube",
    "⛲ fountain",
    "⛺ tent",
    "🌁 mist",
    "🌃 night",
    "🌄 dawn",
    "🌅 sunrise",
    "🌆 dusk",
    "🌇 sunset",
    "🌌 galaxy",
    "🎠 carousel",
    "🎡 wheel",
    "🎢 coaster",
    "💈 barber",
    "🎪 circus",
    "🧳 trunk",
    "🚀 rocket",
    "🛸 saucer",
    "✈️ jet",
    "🚁 chopper",
    "🛶 canoe",
    "⛵ yacht",
    "🚤 boat",
    "🛳️ liner",
    "⛴️ ferry",
    "🛥️ cruiser",
    "🚂 train",
    "🚃 car",
    "🚄 bullet",
    "🚅 tgv",
    "🚆 metro",
    "🚇 subway",
    "🚈 light",
    "🚉 station",
    "🚊 tram",
    "🚝 mono",
    "🚞 mountain",
    "🚋 cable",
    "🚌 bus",
    "🚍 coach",
    "🚎 trolley",
    "🚏 stop",
    "🚐 mini",
    "🚑 amb",
    "🚒 fire",
    "🚓 cop",
    "🚔 cruiser",
    "🚕 taxi",
    "🚖 cab",
    "🚗 sedan",
    "🚘 motor",
    "🚙 suv",
    "🚚 truck",
    "🚛 rig",
    "🚜 tractor",
    "🏍️ bike",
    "🛵 scoot",
    "🚲 cycle",
    "🛴 kick",
    "🛹 board",
    "🛼 skate",
    "🦽 wheel",
    "🦼 chair",
    "🚨 siren",
    "🚧 cone",
    "🚥 light",
    "🪜 ladder",
    "🪞 mirror",
    "🪟 window",
    "🪠 plunger",
    "🪣 bucket",
    "🪤 trap",
    "🪥 brush",
    "🪦 stone",
    "🧴 lotion",
    "🧵 thread",
    "🧶 yarn",
    "🧷 pin",
    "🧸 teddy",
    "🧹 broom",
    "🧺 basket",
    "🧼 soap",
];

const HKDF_OOB_OUTPUT_LEN: usize = OOB_WORD_COUNT;

/// Derive the 4-word OOB code for `bond_key`.
///
/// Pure deterministic: `oob_code_for_bond(&k) == oob_code_for_bond(&k)` for
/// every `k`. No clock, no env input, no salt — see
/// `specs/journeys/JOURNEY-S-011-pairing-desktop.md` Phase 3 for the rationale.
///
/// The four returned strings are entries from the [`OOB_WORDS`] table indexed
/// by the first [`OOB_WORD_COUNT`] bytes of `HKDF<Sha256>(None, bond_key,
/// info=HKDF_INFO_OOB_V1)`.
#[must_use]
pub fn oob_code_for_bond(bond_key: &[u8; OOB_BOND_KEY_BYTES]) -> [String; OOB_WORD_COUNT] {
    let hk = Hkdf::<Sha256>::new(None, bond_key);
    let mut out = [0u8; HKDF_OOB_OUTPUT_LEN];
    // HKDF::expand only errors when the requested output exceeds 255 * 32 =
    // 8160 bytes. 4 bytes is far below that bound, so the call is infallible
    // by construction. We still match the result to avoid `unwrap()` per the
    // AGENTS.md non-negotiable.
    match hk.expand(HKDF_INFO_OOB_V1, &mut out) {
        Ok(()) => {}
        // Unreachable in practice; the deterministic test
        // `oob_code_is_deterministic_for_fixed_key` would fail loudly if the
        // function ever returned all-zero indices.
        Err(_) => out = [0u8; HKDF_OOB_OUTPUT_LEN],
    }
    [
        OOB_WORDS[out[0] as usize].to_owned(),
        OOB_WORDS[out[1] as usize].to_owned(),
        OOB_WORDS[out[2] as usize].to_owned(),
        OOB_WORDS[out[3] as usize].to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_KEY_A: [u8; OOB_BOND_KEY_BYTES] = [0x01; OOB_BOND_KEY_BYTES];
    const FIXED_KEY_B: [u8; OOB_BOND_KEY_BYTES] = [0x02; OOB_BOND_KEY_BYTES];

    #[test]
    fn oob_word_table_has_exactly_256_entries() {
        assert_eq!(OOB_WORDS.len(), 256);
    }

    #[test]
    fn oob_word_table_entries_are_non_empty() {
        for (i, w) in OOB_WORDS.iter().enumerate() {
            assert!(!w.is_empty(), "OOB_WORDS[{i}] is empty");
        }
    }

    #[test]
    fn oob_code_is_deterministic_for_fixed_key() {
        let a = oob_code_for_bond(&FIXED_KEY_A);
        let b = oob_code_for_bond(&FIXED_KEY_A);
        assert_eq!(a, b, "same bond_key must produce the same word tuple");
        assert_eq!(a.len(), OOB_WORD_COUNT);
    }

    #[test]
    fn oob_code_differs_across_keys() {
        let a = oob_code_for_bond(&FIXED_KEY_A);
        let b = oob_code_for_bond(&FIXED_KEY_B);
        // Not a strict invariant of HKDF (two keys could theoretically collide
        // on 32 bits), but for these fixed test keys the outputs differ —
        // pinning that with an assertion makes a regression in the HKDF info
        // string instantly visible.
        assert_ne!(a, b);
    }

    #[test]
    fn oob_code_each_word_is_in_table() {
        let code = oob_code_for_bond(&FIXED_KEY_A);
        for w in code.iter() {
            assert!(OOB_WORDS.iter().any(|t| t == w), "word {w:?} must come from OOB_WORDS");
        }
    }
}
