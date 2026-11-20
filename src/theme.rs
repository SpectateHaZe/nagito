//! Night-drive palette: dusk sky, sodium streetlamp amber, and signal colors reserved for live health.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

pub const DUSK: Color32 = Color32::from_rgb(0x1E, 0x1C, 0x30);
pub const PANEL: Color32 = Color32::from_rgb(0x28, 0x26, 0x40);
pub const PANEL_HI: Color32 = Color32::from_rgb(0x32, 0x2F, 0x4E);
pub const LINE: Color32 = Color32::from_rgb(0x3A, 0x37, 0x56);
pub const LAMP: Color32 = Color32::from_rgb(0xF2, 0xA5, 0x41);
pub const LAMP_HI: Color32 = Color32::from_rgb(0xF7, 0xB9, 0x62);
pub const DEEP: Color32 = Color32::from_rgb(0x14, 0x13, 0x24);
pub const SKY_TOP: Color32 = Color32::from_rgb(0x17, 0x16, 0x2A);
pub const SKY_LOW: Color32 = Color32::from_rgb(0x25, 0x20, 0x3E);
pub const SIGNAL: Color32 = Color32::from_rgb(0x7F, 0xD1, 0xC7);
pub const FLARE: Color32 = Color32::from_rgb(0xE5, 0x73, 0x7A);
pub const PAPER: Color32 = Color32::from_rgb(0xEE, 0xE7, 0xDA);
pub const HAZE: Color32 = Color32::from_rgb(0x9A, 0x96, 0xB4);
pub const SPACE: f32 = 12.0;

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |f: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        f.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(&mut fonts, "barlow", include_bytes!("../assets/fonts/Barlow-Medium.ttf"));
    add(&mut fonts, "barlow-condensed", include_bytes!("../assets/fonts/BarlowCondensed-Bold.ttf"));
    fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "barlow".into());
    fonts.families.insert(FontFamily::Name("display".into()), vec!["barlow-condensed".into(), "barlow".into()]);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(16.0));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(16.0));
        s.spacing.button_padding = egui::vec2(SPACE, SPACE * 0.5);
        s.spacing.interact_size.y = 32.0;
        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = DUSK;
        v.window_fill = PANEL;
        v.extreme_bg_color = PANEL;
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
            w.fg_stroke.color = PAPER;
        }
        v.selection.bg_fill = LAMP;
        v.selection.stroke.color = DUSK;
        v.widgets.inactive.weak_bg_fill = PANEL;
        v.widgets.inactive.bg_fill = DEEP; // slider rail
        v.widgets.hovered.weak_bg_fill = PANEL_HI;
        v.widgets.hovered.bg_fill = PANEL_HI;
        v.widgets.active.weak_bg_fill = PANEL_HI;
        v.widgets.noninteractive.bg_stroke.color = LINE;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, LINE);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0, LAMP);
        v.slider_trailing_fill = true;
    });
}
