use std::{
    fmt::{self, Display, Formatter},
    num::NonZeroU32,
};

use serenity::{
    builder::{CreateEmbed, CreateEmbedFooter},
    model::Colour,
};
use songbird::input::AuxMetadata;

pub(crate) struct TrackInfoKey;

impl songbird::typemap::TypeMapKey for TrackInfoKey {
    type Value = TrackInfo;
}

pub(crate) struct TrackInfo {
    /// Name or title of the track
    name: Box<str>,
    /// Source URL of the track
    source: Box<str>,
    /// Thumbnail url of the track
    thumbnail_url: Option<Box<str>>,
    /// Track duration in seconds if available. For infinite streams it is None
    duration_sec: Option<NonZeroU32>,
    /// Author of the /play command
    added_by: Box<str>,
}

impl TrackInfo {
    pub(crate) fn new(url: String, meta: Option<AuxMetadata>, added_by: String) -> Self {
        let source = url.into_boxed_str();
        let added_by = added_by.into_boxed_str();

        if let Some(meta) = meta {
            let name = meta
                .title
                .map(|name| {
                    if name.starts_with("rt_podcast") {
                        name.replace("rt_podcast", "Радио-Т ")
                    } else {
                        name
                    }
                })
                .map(String::into_boxed_str)
                .unwrap_or_else(|| source.clone());

            Self {
                name,
                source,
                thumbnail_url: meta.thumbnail.map(String::into_boxed_str),
                duration_sec: meta
                    .duration
                    .map(|d| d.as_secs() as u32)
                    .and_then(NonZeroU32::new),
                added_by,
            }
        } else {
            Self {
                name: source.clone(),
                source,
                thumbnail_url: None,
                duration_sec: None,
                added_by,
            }
        }
    }

    /// Creates Discord embed with the track info
    pub(crate) fn into_embed(&self) -> CreateEmbed {
        let mut embed = CreateEmbed::default()
            .description(format!("{self}"))
            .color(Colour::RED)
            .footer(CreateEmbedFooter::new(format!(
                "Added by {}",
                self.added_by
            )));
        if let Some(thumbnail_url) = &self.thumbnail_url {
            embed = embed.thumbnail(thumbnail_url.clone());
        }

        embed
    }
}

impl Display for TrackInfo {
    /// Forms a Markdown link with `[name](source_url)` and duration if available
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]({})", self.name, self.source)?;

        if let Some(duration_secs) = self.duration_sec {
            let mins = duration_secs.get() / 60;
            let secs = duration_secs.get() % 60;
            write!(f, " {mins}:{secs:02}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_track_info_display() {
        assert_eq!(
            format!(
                "{}",
                TrackInfo {
                    name: "Test".into(),
                    source: "https://example.com".into(),
                    thumbnail_url: None,
                    duration_sec: NonZeroU32::new(123),
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
                    name: "Test".into(),
                    source: "https://example.com".into(),
                    thumbnail_url: None,
                    duration_sec: None,
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
                    name: "Нейромонах Феофан — Притоптать | Neuromonakh Feofan".into(),
                    source: "https://www.youtube.com/watch?v=HNpLuXOg7xQ".into(),
                    thumbnail_url: None,
                    duration_sec: NonZeroU32::new(210),
                    added_by: "TestUser".into(),
                }
            ),
            "[Нейромонах Феофан — Притоптать | Neuromonakh Feofan](https://www.youtube.com/watch?v=HNpLuXOg7xQ) 3:30"
        );
    }
}
