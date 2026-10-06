//! Interface translations. Command ids, document text and file names remain stable.
//! Untranslated labels fall back to English so coverage can grow incrementally.
//!
//! Japanese text uses the Japanese craft-fonts when FilmCraft was built with them (`CRAFT_FONTS_DIR`;
//! `theme::install` already puts them in every font family, see [`craft_japanese_font`]), otherwise
//! a font already installed on the system ([`system_japanese_font`]); with neither, switching to
//! Japanese is refused with a message.

use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Ja,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::En, Self::Ja];

    pub fn name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "日本語",
        }
    }

    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }

    pub fn tr(self, text: &str) -> &str {
        if self == Self::Ja
            && let Some((_, japanese)) = JAPANESE.iter().find(|(english, _)| *english == text)
        {
            return japanese;
        }
        text
    }
}

/// Text every Japanese interface font must cover (menus use kanji, hiragana and katakana).
const JAPANESE_SAMPLE: &str = "日本語ファイル編集あア";

/// Installed families preferred for Japanese interface text, best first (Gothic / sans-serif faces
/// read best at menu sizes). Any other installed face that covers [`JAPANESE_SAMPLE`] is used if
/// none of these is present.
const PREFERRED_JAPANESE: &[&str] = &[
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Hiragino Kaku Gothic Pro",
    "Yu Gothic UI",
    "Yu Gothic",
    "Meiryo UI",
    "Meiryo",
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Source Han Sans JP",
    "Source Han Sans",
    "IPAexGothic",
    "IPAGothic",
    "TakaoGothic",
    "VL Gothic",
];

const JAPANESE_FONT: &str = "system-japanese";

/// A Japanese font already installed on this system, for the interface (none is bundled). Looked up
/// once per process: the system font folders are scanned on first use (name tables only), then the
/// chosen face's file is read. `None` on the web and on systems without a Japanese font.
pub fn system_japanese_font() -> Option<Arc<egui::FontData>> {
    static FONT: OnceLock<Option<Arc<egui::FontData>>> = OnceLock::new();
    FONT.get_or_init(|| {
        filmcraft_text::fonts::scan_system();
        let faces: Vec<_> = filmcraft_text::fonts::all_faces().into_iter().filter(|f| f.info.origin == "system" && !f.info.italic).collect();
        let covers = |f: &filmcraft_text::fonts::Face| JAPANESE_SAMPLE.chars().all(|c| f.has_char(c));
        // within a family, the face closest to regular weight
        let by_weight = |f: &&Arc<filmcraft_text::fonts::Face>| f.info.weight.abs_diff(400);
        let preferred =
            PREFERRED_JAPANESE.iter().find_map(|name| faces.iter().filter(|f| f.info.family.eq_ignore_ascii_case(name) && covers(f)).min_by_key(by_weight));
        let face = preferred.or_else(|| faces.iter().filter(|f| covers(f)).min_by_key(by_weight))?;
        // kept for the life of the process (one font, read once) so installing it again after a
        // theme change shares the bytes instead of copying the whole file
        let bytes: &'static [u8] = Box::leak(face.data()?.into_boxed_slice());
        Some(Arc::new(egui::FontData { font: std::borrow::Cow::Borrowed(bytes), index: face.info.index, tweak: Default::default() }))
    })
    .clone()
}

/// Whether the craft-fonts build input (empty unless built with `CRAFT_FONTS_DIR`) supplies a
/// Japanese interface font: some craft-fonts face covers [`JAPANESE_SAMPLE`]. `theme::install` adds
/// these faces to every font family, so nothing else needs installing (and no system scan runs).
pub fn craft_japanese_font() -> bool {
    filmcraft_text::fonts::craft_japanese().next().is_some()
        && filmcraft_text::fonts::all_faces()
            .iter()
            .any(|f| f.info.origin == filmcraft_text::fonts::CRAFT_ORIGIN && JAPANESE_SAMPLE.chars().all(|c| f.has_char(c)))
}

/// Japanese for the interface: true when built with the craft-fonts (already installed by
/// `theme::install`). Otherwise add the system's Japanese font as the last fallback of every theme font family, from the next
/// pass on. Returns false (and changes nothing) when no Japanese font is installed. Call it again
/// after `theme::install`, which replaces the font definitions.
pub fn install_japanese_font(ctx: &egui::Context) -> bool {
    if craft_japanese_font() {
        return true;
    }
    let Some(font) = system_japanese_font() else { return false };
    let families = crate::theme::font_families()
        .into_iter()
        .map(|family| egui::epaint::text::InsertFontFamily { family, priority: egui::epaint::text::FontPriority::Lowest })
        .collect();
    // queued for the next pass (works before the first frame); a no-op when already installed
    ctx.add_font(egui::epaint::text::FontInsert { name: JAPANESE_FONT.into(), data: (*font).clone(), families });
    true
}

const JAPANESE: &[(&str, &str)] = &[
    ("Clip", "クリップ"),
    ("Sequence", "シーケンス"),
    ("Markers", "マーカー"),
    ("Graphics and Titles", "グラフィックスとタイトル"),
    ("Settings", "環境設定"),
    ("Object", "オブジェクト"),
    ("Effect", "効果"),
    ("Settings…", "環境設定…"),
    ("Image", "画像"),
    ("Layer", "レイヤー"),
    ("Type", "書式"),
    ("Select", "選択"),
    ("Filter", "フィルター"),
    ("Window", "ウィンドウ"),
    ("Language", "表示言語"),
    ("Save As…", "別名で保存…"),
    ("Exit", "終了"),
    ("New…", "新規…"),
    ("New", "新規"),
    ("Horizontal", "横書き"),
    ("Vertical", "縦書き"),
    ("Orientation", "組み方向"),
    ("Layers", "レイヤー"),
    ("History", "履歴"),
    ("Properties", "プロパティ"),
    ("Color", "カラー"),
    ("Brush Settings", "ブラシ設定"),
    ("Tools", "ツール"),
    ("Options", "オプション"),
    ("Zoom In", "ズームイン"),
    ("Zoom Out", "ズームアウト"),
    ("Fit on Screen", "画面に合わせる"),
    ("Copy", "コピー"),
    ("Cut", "切り取り"),
    ("Paste", "貼り付け"),
    ("Select All", "すべて選択"),
    ("Deselect", "選択を解除"),
    ("Export", "書き出し"),
    ("Export As…", "形式を指定して書き出し…"),
    ("Search…", "検索…"),
    ("Theme", "テーマ"),
    ("Menu", "メニュー"),
    ("File", "ファイル"),
    ("Edit", "編集"),
    ("Pages", "ページ"),
    ("View", "表示"),
    ("Help", "ヘルプ"),
    ("Preferences", "環境設定"),
    ("Preferences…", "環境設定…"),
    ("Interface language", "表示言語"),
    ("Open…", "開く…"),
    ("New blank PDF", "空白の PDF を作成"),
    ("Create PDF from file…", "ファイルから PDF を作成…"),
    ("Create PDF from images…", "画像から PDF を作成…"),
    ("Create PDF from clipboard", "クリップボードから PDF を作成"),
    ("Combine files…", "ファイルを結合…"),
    ("Save", "保存"),
    ("Save as…", "別名で保存…"),
    ("Close file", "ファイルを閉じる"),
    ("Close all", "すべて閉じる"),
    ("Revert", "保存済みの状態に戻す"),
    ("Print…", "印刷…"),
    ("Document properties…", "文書のプロパティ…"),
    ("Undo", "取り消し"),
    ("Redo", "やり直し"),
    ("Find…", "検索…"),
    ("Advanced search…", "高度な検索…"),
    ("Copy pages", "ページをコピー"),
    ("Cut pages", "ページを切り取り"),
    ("Paste pages", "ページを貼り付け"),
    ("Fit visible", "表示範囲に合わせる"),
    ("Marquee zoom", "範囲指定ズーム"),
    ("Take a snapshot", "スナップショットを作成"),
    ("Full screen mode", "全画面表示"),
    ("Read mode", "閲覧モード"),
    ("Switch light / dark theme", "明るい／暗いテーマを切り替え"),
    ("Comments panel", "コメントパネル"),
    ("Form fields panel", "フォームフィールドパネル"),
    ("Clear form", "フォームをクリア"),
    ("Find tools and commands…", "ツールとコマンドを検索…"),
    ("Zoom", "ズーム"),
    ("Actual size", "実際のサイズ"),
    ("Zoom to page level", "ページ全体を表示"),
    ("Fit to width", "幅に合わせる"),
    ("Display theme", "表示テーマ"),
    ("Side panels", "サイドパネル"),
    ("OK", "OK"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translations_are_unique_and_preserve_unknown_text() {
        for (i, (en, ja)) in JAPANESE.iter().enumerate() {
            assert!(!ja.is_empty());
            assert!(JAPANESE.iter().take(i).all(|(other, _)| en != other));
            assert_eq!(Language::En.tr(en), *en);
        }
        assert_eq!(Language::Ja.tr("File"), "ファイル");
        assert_eq!(Language::Ja.tr("日本語の文書.pdf"), "日本語の文書.pdf");
        assert_eq!(Language::parse("xx"), None);
    }

    #[test]
    fn language_commands_switch_and_persist_without_a_document() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::Ja);
        let saved = serde_json::to_string(&app.ui).unwrap();
        let restored: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.language, Language::Ja);
        crate::menus::invoke(&mut app, &ctx, "app.language.english", serde_json::json!({})).unwrap();
        assert_eq!(app.ui.language, Language::En);
    }

    #[test]
    fn japanese_needs_an_installed_font() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &crate::theme::Tokens::for_kind(crate::theme::ThemeKind::default()));
        let r = crate::menus::invoke(&mut app, &ctx, "app.language.japanese", serde_json::json!({}));
        let craft = craft_japanese_font();
        if !craft && system_japanese_font().is_none() {
            // no craft-fonts and no system font: refused, and the interface stays English
            assert!(r.is_err(), "{r:?}");
            assert_eq!(app.ui.language, Language::En);
            return;
        }
        assert!(r.is_ok(), "{r:?}");
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        ctx.fonts_mut(|fonts| {
            // every theme family falls back to the craft-fonts (when built with them; no system
            // font is added then) or to the system Japanese font
            for family in crate::theme::font_families() {
                let stack = fonts.definitions().families.get(&family).cloned().unwrap_or_default();
                if craft {
                    assert!(stack.last().is_some_and(|n| n.starts_with("craft:")), "{family:?}: {stack:?}");
                    assert!(!stack.iter().any(|n| n == JAPANESE_FONT), "{family:?}: {stack:?}");
                } else {
                    assert_eq!(stack.last().map(String::as_str), Some(JAPANESE_FONT), "{family:?}: {stack:?}");
                }
            }
            // and the glyphs resolve. (Only families whose replacement-box face is another font:
            // egui's `has_glyph` reports false for any character served by the face it also uses
            // for the replacement box, which in the Inter-only "medium"/"semibold" stacks is the
            // Japanese font itself.)
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                let font = egui::FontId::new(13.0, family);
                for ch in JAPANESE_SAMPLE.chars() {
                    assert!(fonts.has_glyph(&font, ch), "missing {ch} in {font:?}");
                }
            }
        });
    }
}
