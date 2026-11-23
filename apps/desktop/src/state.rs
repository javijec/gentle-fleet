use crate::{navigation::Page, theme::ThemeMode};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopState {
    pub page: Page,
    pub sidebar_open: bool,
    pub theme: ThemeMode,
    pub sidebar_width: u16,
}

impl Default for DesktopState {
    fn default() -> Self {
        Self {
            page: Page::Welcome,
            sidebar_open: true,
            theme: ThemeMode::Dark,
            sidebar_width: 248,
        }
    }
}

impl DesktopState {
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_open = !self.sidebar_open;
    }

    pub fn navigate(&mut self, page: Page) {
        self.page = page;
    }

    pub fn set_theme(&mut self, theme: ThemeMode) {
        self.theme = theme;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_on_welcome_with_empty_fleet_and_no_session() {
        let state = DesktopState::default();

        assert_eq!(state.page, Page::Welcome);
        assert!(state.sidebar_open);
        assert_eq!(state.sidebar_width, 248);
    }

    #[test]
    fn navigation_changes_page_without_creating_fleet_state() {
        let mut state = DesktopState::default();

        state.navigate(Page::Settings);
        assert_eq!(state.page, Page::Settings);

        state.navigate(Page::SessionInformation);
        assert_eq!(state.page, Page::SessionInformation);
    }

    #[test]
    fn sidebar_can_be_opened_and_closed() {
        let mut state = DesktopState::default();

        state.toggle_sidebar();
        assert!(!state.sidebar_open);
        state.toggle_sidebar();
        assert!(state.sidebar_open);
    }

    #[test]
    fn theme_selection_is_applied_without_changing_navigation() {
        let mut state = DesktopState::default();

        state.set_theme(ThemeMode::Light);
        assert_eq!(state.theme, ThemeMode::Light);
        assert_eq!(state.page, Page::Welcome);
    }

    #[test]
    fn opening_session_information_is_only_a_navigation_transition() {
        let mut state = DesktopState::default();

        state.navigate(Page::SessionInformation);
        assert_eq!(state.page, Page::SessionInformation);
        assert_eq!(
            state,
            DesktopState {
                page: Page::SessionInformation,
                ..DesktopState::default()
            }
        );
    }
}
