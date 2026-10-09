// Data shapes produced by engine/inject.js (see the `item` / `snapshot` helpers there).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<f64>,
    pub artwork: Option<String>,
    pub catalog_id: Option<String>,
    pub track_count: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub release_date: Option<String>,
    pub genre: Option<String>,
    pub explicit: bool,
    pub is_live: bool,
    pub note: Option<String>,
    /// Queue only: appended by autoplay rather than by the user.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub autoplay: bool,
    /// Library pins only: play | shuffle | drillIn (open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin_action: Option<String>,
    /// Library playlists the user may edit (their own).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub can_edit: bool,
}

/// What an item is, regardless of catalog / library flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Song,
    Album,
    Playlist,
    Station,
    Artist,
    Other,
}

impl Item {
    pub fn title(&self) -> &str {
        self.title.as_deref().unwrap_or("Untitled")
    }

    pub fn artist(&self) -> &str {
        self.artist.as_deref().unwrap_or("")
    }

    pub fn kind(&self) -> Kind {
        let k = self.kind.as_deref().unwrap_or("");
        let k = k.strip_prefix("library-").unwrap_or(k);
        match k.trim_end_matches('s') {
            "song" | "music-video" => Kind::Song,
            "album" => Kind::Album,
            "playlist" => Kind::Playlist,
            "station" => Kind::Station,
            "artist" => Kind::Artist,
            _ => Kind::Other,
        }
    }

    pub fn is_library(&self) -> bool {
        self.kind.as_deref().is_some_and(|k| k.starts_with("library-"))
    }

    /// Id to queue a song with: the catalog id when the library entry has one.
    pub fn song_play_id(&self) -> String {
        self.catalog_id.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Catalog song id, if there is one (lyrics, ratings, links).
    pub fn catalog_song_id(&self) -> Option<String> {
        let id = self.song_play_id();
        (!id.starts_with("i.")).then_some(id)
    }

    pub fn art(&self, px: u32) -> Option<String> {
        self.artwork.as_deref().map(|t| art_url(t, px))
    }

    pub fn year(&self) -> Option<&str> {
        self.release_date.as_deref().and_then(|d| d.get(..4))
    }
}

/// Apple artwork URLs are templates: .../{w}x{h}{c}.{f}
pub fn art_url(template: &str, px: u32) -> String {
    let px = px.to_string();
    template
        .replace("{w}", &px)
        .replace("{h}", &px)
        .replace("{c}", "bb")
        .replace("{f}", "jpg")
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    /// Engine event counter; replies older than the last event are stale.
    pub seq: Option<u64>,
    pub state: String,
    pub authorized: bool,
    pub position: Option<f64>,
    pub duration: Option<f64>,
    pub volume: Option<f64>,
    pub now_playing: Option<Item>,
    pub repeat: Option<u8>,
    pub shuffle: Option<bool>,
    pub autoplay: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryPage {
    pub items: Vec<Item>,
    pub next: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    pub songs: Vec<Item>,
    pub albums: Vec<Item>,
    pub artists: Vec<Item>,
    pub playlists: Vec<Item>,
    pub stations: Vec<Item>,
}

/// A shelf on Home / Browse / Radio / artist pages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Section {
    pub title: String,
    pub items: Vec<Item>,
    pub hero: bool,
    pub songs: bool,
    pub key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistPage {
    pub artist: Item,
    pub bio: Option<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContainerPage {
    pub info: Item,
    pub tracks: Vec<Item>,
    pub artist_id: Option<String>,
    pub catalog_id: Option<String>,
    pub copyright: Option<String>,
    pub description: Option<String>,
    pub in_library: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SongLinks {
    pub song_id: Option<String>,
    pub album: Option<Item>,
    pub artist_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricLine {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub text: String,
    /// Syllable-synced lyrics: every syllable with its timing (text keeps trailing spaces).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Syllable>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Syllable {
    pub start: Option<f64>,
    pub end: Option<f64>,
    pub text: String,
    /// Background vocals.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bg: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Lyrics {
    pub synced: bool,
    pub syllable: bool,
    pub lines: Vec<LyricLine>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Queue {
    pub position: i64,
    pub items: Vec<Item>,
    pub autoplay: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Suggestions {
    pub terms: Vec<String>,
    pub top: Vec<Item>,
}

pub fn fmt_time(s: f64) -> String {
    if !s.is_finite() || s < 0.0 {
        return "0:00".into();
    }
    let s = s as u64;
    if s >= 3600 {
        return format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
    }
    format!("{}:{:02}", s / 60, s % 60)
}

/// "1 song" / "2 songs".
pub fn count(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}
