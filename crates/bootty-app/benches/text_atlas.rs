use std::{hint::black_box, sync::Arc};

use bootty_app::{
    geometry::SurfaceRect,
    paint_plan::{PlanColor, TextAttrs},
    terminal_render::TextCommand,
    terminal_text::{
        CodepointPresentation, FontResolver, FontStyle, TerminalCodepointResolver,
        TerminalTextConfig,
    },
    terminal_text_atlas::{GlyphAtlas, TerminalTextShaper, TextAtlasBuilder},
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use libghostty_vt::style::Underline;

fn attrs() -> TextAttrs {
    TextAttrs {
        fg: PlanColor {
            r: 220,
            g: 221,
            b: 222,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn text_command(text: impl Into<String>) -> TextCommand {
    let text = text.into();
    TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, text.chars().count() as f32 * 9.0, 22.0),
        text,
        attrs: attrs(),
        face: Arc::new(FontResolver::new(TerminalTextConfig::default()).resolve_face(&attrs())),
        font_size: TerminalTextConfig::default().font_size,
        font_features: Arc::from(TerminalTextConfig::default().font_features),
    }
}

fn ascii_text() -> String {
    "the quick brown fox jumps over the lazy dog 0123456789 ".repeat(8)
}

fn unicode_text() -> String {
    "unicode café e\u{301} Ω界 λ∑→← 🥟 🚀 main \u{F126} ".repeat(6)
}

fn emoji_text() -> String {
    "emoji 🥟 🚀 🔥 ✨ ✅ 🧪 🧵 terminal ".repeat(8)
}

fn combining_text() -> String {
    "e\u{301} a\u{0308} o\u{0302} n\u{0303} c\u{0327} ".repeat(24)
}

fn complex_script_text() -> String {
    "مرحبا بالعالم नमस्ते दुनिया שלום עולם สวัสดีโลก ".repeat(6)
}

fn latin1_text() -> String {
    "Latin-1 àáâä æ ç èéêë ñ ø ü ß ¡¿ currency £¥€ ".repeat(8)
}

fn greek_cyrillic_text() -> String {
    "Greek Ελληνικά αβγδεζη Cyrillic Кириллица абвгдеж ".repeat(8)
}

fn cjk_text() -> String {
    "CJK 日本語 中文 한국어 界面 終端 仮名 カタカナ 漢字 ".repeat(8)
}

fn ambiguous_width_text() -> String {
    "ambiguous ·×÷±§¶©®™←↑→↓⇐⇒⇔ ∑√∞≈≠≤≥ ".repeat(10)
}

fn box_powerline_nerd_text() -> String {
    "box ┌─┬─┐│┃╋╬╠╣╦╩█▇▆▅▄▃▂▁ powerline  nerd    ".repeat(6)
}

fn emoji_skin_tone_text() -> String {
    "emoji skin tones 👍 👍🏻 👍🏼 👍🏽 👍🏾 👍🏿 👋🏽 🧑🏾‍💻 ".repeat(8)
}

fn emoji_zwj_flags_text() -> String {
    "emoji zwj 👨‍👩‍👧‍👦 👩‍❤️‍👨 🧑‍🚀 🏳️‍🌈 flags 🇺🇸 🇯🇵 🇧🇷 🇺🇳 ".repeat(6)
}

fn variation_selector_text() -> String {
    "variation selectors ❤︎ ❤️ ☺︎ ☺️ 0️⃣ 1️⃣ ™︎ ™️ ©︎ ©️ ".repeat(10)
}

fn zalgo_text() -> String {
    "Zalgo z̵̦͋a̸͎͛l̶̬̎g̷̦͆o̶̮͌ e̴̥̓v̷̬̓i̷̱͋l̴͕͊ ".repeat(8)
}

fn rtl_text() -> String {
    "Arabic العربية مرحبا بالعالم Hebrew עברית שלום עולם ".repeat(8)
}

fn devanagari_text() -> String {
    "Devanagari नमस्ते दुनिया हिन्दी संस्कृत क्ष त्र ज्ञ ".repeat(8)
}

fn ligature_source_text() -> String {
    "fn main() -> Result<()> { value != other && ready || fallback <= limit >= min => ok } "
        .repeat(6)
}

fn missing_fallback_text() -> String {
    "missing/fallback 𐍈 𐎀 𒀱 𓀀 𝌆 𝄞 🜁 🝗 private \u{E0B0} \u{F126} ".repeat(6)
}

fn unicode_workloads() -> Vec<(&'static str, String)> {
    vec![
        ("ascii", ascii_text()),
        ("latin1", latin1_text()),
        ("greek_cyrillic", greek_cyrillic_text()),
        ("cjk", cjk_text()),
        ("ambiguous_width", ambiguous_width_text()),
        ("box_powerline_nerd", box_powerline_nerd_text()),
        ("unicode_fallback", unicode_text()),
        ("emoji", emoji_text()),
        ("emoji_skin_tone", emoji_skin_tone_text()),
        ("emoji_zwj_flags", emoji_zwj_flags_text()),
        ("variation_selectors", variation_selector_text()),
        ("combining_marks", combining_text()),
        ("zalgo", zalgo_text()),
        ("rtl_arabic_hebrew", rtl_text()),
        ("devanagari", devanagari_text()),
        ("complex_scripts", complex_script_text()),
        ("ligature_source", ligature_source_text()),
        ("missing_fallback", missing_fallback_text()),
    ]
}

fn atlas_growth_commands() -> Vec<TextCommand> {
    (0..192)
        .map(|index| {
            let ch = char::from_u32(0x2500 + (index % 128)).unwrap_or('─');
            text_command(format!("glyph-{index:03}-{ch}"))
        })
        .collect()
}

fn prepare_command(builder: &mut TextAtlasBuilder, command: &TextCommand) -> usize {
    let mut quads = Vec::new();
    builder.prepare_text_command_into(command, 1.0, &mut quads);
    quads.len()
}

fn bench_text_shaping(c: &mut Criterion) {
    let shaper = TerminalTextShaper;
    for (name, text) in unicode_workloads() {
        c.bench_function(&format!("text_shape_cold_{name}"), |b| {
            b.iter(|| black_box(shaper.shape(black_box(&text), 0).len()))
        });

        c.bench_function(&format!("text_shape_warm_into_{name}"), |b| {
            let mut clusters = Vec::new();
            shaper.shape_into(&text, 0, &mut clusters);
            b.iter(|| black_box(shaper.shape_into(black_box(&text), 0, &mut clusters)))
        });
    }
}

fn bench_font_resolution(c: &mut Criterion) {
    let mut config = TerminalTextConfig::default();
    config.codepoint_overrides.add('🥟'..='🥟', "Emoji Color");
    config.codepoint_overrides.add('界'..='界', "CJK Override");
    let resolver = FontResolver::new(config.clone());
    let codepoint_resolver = TerminalCodepointResolver::new(config);
    let attrs = attrs();

    c.bench_function("font_resolve_face_ascii_warm", |b| {
        b.iter(|| black_box(resolver.resolve_face_for_text(black_box(&attrs), "regular text")))
    });
    c.bench_function("font_resolve_face_override_emoji", |b| {
        b.iter(|| black_box(resolver.resolve_face_for_text(black_box(&attrs), "🥟")))
    });
    c.bench_function("font_resolve_codepoint_mixed_batch", |b| {
        let chars = ['a', '界', '🥟', '─', '\u{F126}', 'Ω'];
        b.iter(|| {
            let mut resolved = 0_usize;
            for ch in chars {
                if codepoint_resolver
                    .resolve(ch, FontStyle::Regular, CodepointPresentation::Any)
                    .is_some()
                {
                    resolved += 1;
                }
            }
            black_box(resolved)
        })
    });
}

fn bench_text_atlas(c: &mut Criterion) {
    for (name, text) in unicode_workloads() {
        let command = text_command(text);
        c.bench_function(&format!("text_atlas_cold_prepare_{name}"), |b| {
            b.iter_batched(
                || TextAtlasBuilder::new_rgba(1024, 1024),
                |mut builder| black_box(prepare_command(&mut builder, &command)),
                BatchSize::SmallInput,
            )
        });

        c.bench_function(&format!("text_atlas_warm_reuse_{name}"), |b| {
            let mut builder = TextAtlasBuilder::new_rgba(1024, 1024);
            prepare_command(&mut builder, &command);
            b.iter(|| black_box(prepare_command(&mut builder, black_box(&command))))
        });
    }

    c.bench_function("text_atlas_growth_192_commands", |b| {
        let commands = atlas_growth_commands();
        b.iter_batched(
            || TextAtlasBuilder::new_rgba(256, 256),
            |mut builder| {
                let mut quads = 0_usize;
                for command in &commands {
                    quads += prepare_command(&mut builder, command);
                }
                black_box((builder.atlas_len(), quads))
            },
            BatchSize::LargeInput,
        )
    });
}

fn bench_glyph_atlas_reserve(c: &mut Criterion) {
    // Guards the render-thread freeze: once the atlas saturated, the old fallback scanned every
    // pixel — O(width * height * allocations) — for each glyph that no longer fit, so a full
    // atlas spent seconds per frame here (see the zoom freeze sample). The atlas is filled to
    // saturation in setup (excluded from the measurement); the measured loop then reserves in the
    // saturated state, which the fix answers in O(1) via the saturation memo. A regression back to
    // the per-pixel scan turns these microseconds into milliseconds — a stark dashboard spike.
    c.bench_function("glyph_atlas_reserve_saturated", |b| {
        b.iter_batched(
            || {
                let mut atlas = GlyphAtlas::new(256, 256);
                while atlas.reserve(12, 12).is_some() {}
                atlas
            },
            |mut atlas| {
                let mut misses = 0_u32;
                for _ in 0..256 {
                    if atlas.reserve(black_box(12), black_box(12)).is_none() {
                        misses += 1;
                    }
                }
                black_box(misses)
            },
            BatchSize::LargeInput,
        )
    });

    // The gap-reclaiming first-fit: against an atlas already holding many allocations, a small
    // glyph still fits a gap the shelf packer skipped. The fix visits only allocation-edge
    // candidates; the old scan walked the whole surface to reach the same slot.
    c.bench_function("glyph_atlas_reserve_gap_fill", |b| {
        b.iter_batched(
            || {
                let mut atlas = GlyphAtlas::new(256, 256);
                while atlas.reserve(12, 20).is_some() {}
                atlas
            },
            |mut atlas| black_box(atlas.reserve(black_box(4), black_box(4))),
            BatchSize::LargeInput,
        )
    });
}

criterion_group!(
name = benches;
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_text_shaping,
    bench_font_resolution,
    bench_text_atlas,
    bench_glyph_atlas_reserve
);
criterion_main!(benches);
