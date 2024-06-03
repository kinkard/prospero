use std::{
    fmt::{self, Display, Formatter},
    num::NonZeroU32,
};

use serenity::{
    builder::{CreateEmbed, CreateEmbedFooter},
    model::Colour,
};
use songbird::input::AuxMetadata;

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) struct Metadata {
    /// Track title
    pub(crate) title: Box<str>,
    /// Source URL of the track. Usually a URL to the track page on the platform
    pub(crate) source_url: Box<str>,
    /// Thumbnail url of the track if available
    pub(crate) thumbnail_url: Option<Box<str>>,
    /// Track duration in seconds if available. For infinite streams it is None
    pub(crate) duration_sec: Option<NonZeroU32>,
}

impl Display for Metadata {
    /// Forms a Markdown link with `[title](source_url)` and duration if available
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]({})", self.title, self.source_url)?;

        if let Some(duration_secs) = self.duration_sec {
            let mins = duration_secs.get() / 60;
            let secs = duration_secs.get() % 60;
            write!(f, " {mins}:{secs:02}")?;
        }
        Ok(())
    }
}

impl From<Metadata> for AuxMetadata {
    fn from(meta: Metadata) -> Self {
        Self {
            title: Some(meta.title.into()),
            source_url: Some(meta.source_url.into()),
            thumbnail: meta.thumbnail_url.map(Into::into),
            duration: meta
                .duration_sec
                .map(|d| std::time::Duration::from_secs(d.get() as u64)),
            ..Default::default()
        }
    }
}

pub(crate) struct TrackInfoKey;

impl songbird::typemap::TypeMapKey for TrackInfoKey {
    type Value = TrackInfo;
}

#[cfg_attr(test, derive(Debug, PartialEq))]
pub(crate) struct TrackInfo {
    /// Track metadata
    metadata: Metadata,
    /// Author of the /play command
    added_by: Box<str>,
}

impl TrackInfo {
    pub(crate) fn new(metadata: Metadata, added_by: String) -> Self {
        Self {
            metadata,
            added_by: added_by.into_boxed_str(),
        }
    }

    /// Creates Discord embed with the track info
    pub(crate) fn build_embed(&self) -> CreateEmbed {
        let mut embed = CreateEmbed::default()
            .description(format!("{self}"))
            .color(Colour::RED)
            .footer(CreateEmbedFooter::new(format!(
                "Added by {}",
                self.added_by
            )));
        if let Some(thumbnail_url) = &self.metadata.thumbnail_url {
            embed = embed.thumbnail(thumbnail_url.clone());
        }

        embed
    }
}

impl Display for TrackInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_track_info_display() {
        assert_eq!(
            format!(
                "{}",
                TrackInfo {
                    metadata: Metadata {
                        title: "Test".into(),
                        source_url: "https://example.com".into(),
                        thumbnail_url: None,
                        duration_sec: NonZeroU32::new(123),
                    },
                    added_by: "TestUser".into(),
                }
            ),
            "[Test](https://example.com) 2:03"
        );

        // No duration
        assert_eq!(
            format!(
                "{}",
                TrackInfo {
                    metadata: Metadata {
                        title: "Test".into(),
                        source_url: "https://example.com".into(),
                        thumbnail_url: None,
                        duration_sec: None,
                    },
                    added_by: "TestUser".into(),
                }
            ),
            "[Test](https://example.com)"
        );

        // multi-byte characters in name
        assert_eq!(
            format!(
                "{}",
                TrackInfo {
                    metadata: Metadata {
                        title: "Нейромонах Феофан — Притоптать | Neuromonakh Feofan".into(),
                        source_url: "https://www.youtube.com/watch?v=HNpLuXOg7xQ".into(),
                        thumbnail_url: None,
                        duration_sec: NonZeroU32::new(210),
                    },
                    added_by: "TestUser".into(),
                }
            ),
            "[Нейромонах Феофан — Притоптать | Neuromonakh Feofan](https://www.youtube.com/watch?v=HNpLuXOg7xQ) 3:30"
        );
    }
}
