//! Internal `paper://` links (#21): stable, ID-only references to a paper, an attachment page
//! or an annotation. They never carry titles, text or file paths.
//!
//! ```text
//! paper://<paper-uuid>
//! paper://<paper-uuid>/attachment/<attachment-uuid>
//! paper://<paper-uuid>/attachment/<attachment-uuid>/page/<1-based display page>
//! paper://<paper-uuid>/annotation/<annotation-uuid>
//! ```
use cm_protocol::domain::{AttachmentId, PaperId};

pub const SCHEME: &str = "paper://";
const MAX_LINK_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Paper,
    Attachment(AttachmentId),
    /// 0-based page index (the URI carries the 1-based display number).
    Page(AttachmentId, u32),
    Annotation(uuid::Uuid),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeepLink {
    pub paper: PaperId,
    pub target: Target,
}

impl DeepLink {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.len() > MAX_LINK_BYTES || !text.is_ascii() {
            return None;
        }
        let rest = text.strip_prefix(SCHEME)?;
        let parts: Vec<&str> = rest.split('/').collect();
        let uuid = |s: &str| uuid::Uuid::parse_str(s).ok().filter(|u| !u.is_nil());
        let paper = PaperId(uuid(parts.first()?)?);
        let target = match parts.as_slice() {
            [_] => Target::Paper,
            [_, "attachment", a] => Target::Attachment(AttachmentId(uuid(a)?)),
            [_, "attachment", a, "page", n] => {
                let display: u32 = n.parse().ok().filter(|n: &u32| *n >= 1)?;
                // Display numbers fit the 0-based PageIndex range (max 4294967294).
                Target::Page(AttachmentId(uuid(a)?), display.checked_sub(1)?)
            }
            [_, "annotation", n] => Target::Annotation(uuid(n)?),
            _ => return None,
        };
        Some(Self { paper, target })
    }
}
impl std::fmt::Display for DeepLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{SCHEME}{}", self.paper.0)?;
        match self.target {
            Target::Paper => Ok(()),
            Target::Attachment(a) => write!(f, "/attachment/{}", a.0),
            Target::Page(a, page) => write!(f, "/attachment/{}/page/{}", a.0, page + 1),
            Target::Annotation(n) => write!(f, "/annotation/{n}"),
        }
    }
}
