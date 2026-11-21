use std::time::{Duration, Instant};

use anyhow::Result;

use super::browser::Browser;
use super::decide::Input;
#[cfg(target_os = "macos")]
use super::mac::Mac;
use super::screen::{Element, Node};

pub(super) const POLL_EVERY: Duration = Duration::from_millis(25);
pub(super) const FIRST_CHANGE: Duration = Duration::from_millis(500);
const QUIET: Duration = Duration::from_millis(100);
const SETTLE_WITHIN: Duration = Duration::from_secs(3);
pub(super) const LOAD_WITHIN: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Action {
    Press(Element),
    Spot(f64, f64),
    Hover(Element),
    Type(Element, Input),
    Choose(Element, String),
    Enter,
    Escape,
    Scroll,
    Wait,
}

pub(super) enum Surface {
    Browser(Browser),
    #[cfg(target_os = "macos")]
    Mac(Mac),
}

pub(super) struct Probe<T> {
    pub(super) revision: Option<T>,
    pub(super) busy: bool,
    pub(super) loading: bool,
}

impl Action {
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Press(element) => format!("pressed {}", element.describe()),
            Self::Spot(x, y) => format!(
                "pressed the spot {:.0}% across and {:.0}% down the screen",
                x * 100.0,
                y * 100.0
            ),
            Self::Hover(element) => format!("pointed at {}", element.describe()),
            Self::Type(element, input) => {
                format!("typed {} into {}", input.shown(), element.describe())
            }
            Self::Choose(element, option) => format!("chose {option:?} in {}", element.describe()),
            Self::Enter => "pressed Enter".into(),
            Self::Escape => "pressed Escape".into(),
            Self::Scroll => "scrolled down".into(),
            Self::Wait => "waited".into(),
        }
    }
}

impl Surface {
    pub(super) async fn observe(&mut self) -> Result<Vec<Node>> {
        match self {
            Self::Browser(browser) => browser.observe().await,
            #[cfg(target_os = "macos")]
            Self::Mac(mac) => mac.observe().await,
        }
    }

    pub(super) async fn can_scroll(&self) -> Result<bool> {
        match self {
            Self::Browser(browser) => Ok(browser
                .scroll_point()
                .await?
                .is_some_and(|(_, _, known)| known)),
            #[cfg(target_os = "macos")]
            Self::Mac(_) => Ok(false),
        }
    }

    pub(super) async fn act(&mut self, action: &Action) -> Result<()> {
        match self {
            Self::Browser(browser) => browser.act(action).await,
            #[cfg(target_os = "macos")]
            Self::Mac(mac) => mac.act(action).await,
        }
    }

    pub(super) async fn close(self) {
        match self {
            Self::Browser(browser) => browser.close().await,
            #[cfg(target_os = "macos")]
            Self::Mac(_) => {}
        }
    }
}

pub(super) async fn settle<T: PartialEq>(
    before: Option<T>,
    started: Instant,
    mut probe: impl AsyncFnMut() -> Probe<T>,
) {
    let (mut last, mut changed_at, mut slow) = (before, None::<Instant>, false);
    loop {
        let Probe {
            revision,
            busy,
            loading,
        } = probe().await;
        if busy || revision != last {
            last = revision;
            changed_at = Some(Instant::now());
        }
        let settled = match changed_at {
            Some(at) => at.elapsed() >= QUIET,
            None => started.elapsed() >= FIRST_CHANGE,
        };
        slow |= loading;
        let within = if slow { LOAD_WITHIN } else { SETTLE_WITHIN };
        if settled || started.elapsed() >= within {
            return;
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_after_a_slow_load_are_still_waited_for() {
        let loaded = Instant::now();
        let started = loaded.checked_sub(SETTLE_WITHIN + QUIET).unwrap();
        let mut probes = 0;
        settle(None::<u8>, started, async || {
            probes += 1;
            Probe {
                revision: None,
                busy: probes == 1 || loaded.elapsed() < QUIET * 3,
                loading: probes == 1,
            }
        })
        .await;
        assert!(loaded.elapsed() >= QUIET * 3);
    }
}
