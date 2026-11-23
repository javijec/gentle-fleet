use gpui::{div, prelude::*, px, Div};

use crate::{state::DesktopState, theme::Palette};

pub fn sidebar(state: &DesktopState, palette: Palette) -> Div {
    div()
        .w(px(f32::from(state.sidebar_width)))
        .h_full()
        .flex()
        .flex_col()
        .gap_4()
        .border_r_1()
        .border_color(palette.border)
        .bg(palette.panel)
        .p_4()
        .child(div().text_xs().text_color(palette.muted).child("MACHINES"))
        .child(
            div()
                .rounded(px(8.0))
                .border_1()
                .border_color(palette.border)
                .p_3()
                .text_sm()
                .text_color(palette.muted)
                .child("Sin hosts conectados"),
        )
        .child(div().text_xs().text_color(palette.muted).child("SESIONES"))
        .child(
            div()
                .flex_1()
                .text_sm()
                .text_color(palette.muted)
                .child("Las sesiones aparecerán aquí."),
        )
}
