use anyhow::{Result, anyhow, bail};
use rand::seq::SliceRandom;
use serde_json::Value;
// left for debug
// use std::io::Write;
use reqwest::{Client, ClientBuilder, StatusCode};
use std::time::Duration;
use ytd_rs::{Arg, YoutubeDL};

const INVIDIOUS_INSTANCE_LIST: [&str; 5] = [
    "https://inv.nadeko.net",
    "https://invidious.nerdvpn.de",
    "https://yewtu.be",
    // "https://inv.riverside.rocks",
    // "https://invidious.osi.kr",
    // "https://youtube.076.ne.jp",
    "https://y.com.sb",
    "https://yt.artemislena.eu",
    // "https://invidious.tiekoetter.com",
    // Below lines are left for testing
    // "https://www.google.com",
    // "https://www.google.com",
    // "https://www.google.com",
    // "https://www.google.com",
    // "https://www.google.com",
    // "https://www.google.com",
    // "https://www.google.com",
];

const INVIDIOUS_DOMAINS: &str = "https://api.invidious.io/instances.json?sort_by=type,users";

#[derive(Clone, Debug)]
pub struct Instance {
    pub domain: Option<String>,
    client: Client,
    query: Option<String>,
}

impl PartialEq for Instance {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
    }
}

impl Eq for Instance {}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct YoutubeVideo {
    pub title: String,
    pub length_seconds: u64,
    pub video_id: String,
}

impl Default for Instance {
    fn default() -> Self {
        let client = Client::new();
        let domain = Some(String::new());
        let query = Some(String::new());

        Self {
            domain,
            client,
            query,
        }
    }
}

impl Instance {
    pub async fn new(query: &str) -> Result<(Self, Vec<YoutubeVideo>)> {
        // Use yt-dlp for search with flat-playlist option
        let video_result = Self::search_with_ytdlp(query, 1).await?;
        
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()?;

        Ok((
            Self {
                domain: Some("yt-dlp".to_string()),
                client,
                query: Some(query.to_string()),
            },
            video_result,
        ))
    }

    /// Search YouTube using yt-dlp with --flat-playlist for fast metadata-only search
    async fn search_with_ytdlp(query: &str, page: u32) -> Result<Vec<YoutubeVideo>> {
        // yt-dlp doesn't have native pagination, so we fetch more results and skip based on page
        const RESULTS_PER_PAGE: u32 = 20;
        let total_results = page * RESULTS_PER_PAGE;
        
        let search_query = format!("ytsearch{total_results}:{query}");
        let temp_dir = std::env::temp_dir();
        
        let args = vec![
            Arg::new("--flat-playlist"),
            Arg::new("--dump-json"),
            Arg::new("--skip-download"),
            Arg::new("--no-warnings"),
        ];
        
        let ytd = YoutubeDL::new(&temp_dir, args, &search_query)?;
        
        // Run yt-dlp in a blocking task since it's synchronous
        let result = tokio::task::spawn_blocking(move || ytd.download()).await??;
        
        // Parse the output - each line is a JSON object
        let output = result.output();
        let mut videos = Vec::new();
        
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                if let Some(video) = Self::parse_ytdlp_item(&value) {
                    videos.push(video);
                }
            }
        }
        
        // Return only the last page of results for pagination
        let start_idx = ((page - 1) * RESULTS_PER_PAGE) as usize;
        let videos: Vec<YoutubeVideo> = videos.into_iter().skip(start_idx).collect();
        
        Ok(videos)
    }
    
    /// Parse a single video entry from yt-dlp JSON output
    fn parse_ytdlp_item(value: &Value) -> Option<YoutubeVideo> {
        let title = value.get("title")?.as_str()?.to_owned();
        let video_id = value.get("id")?.as_str()?.to_owned();
        let length_seconds = value.get("duration")?.as_u64().unwrap_or(0);
        
        Some(YoutubeVideo {
            title,
            length_seconds,
            video_id,
        })
    }

    // GetSearchQuery fetches query result from yt-dlp for the specified page.
    pub async fn get_search_query(&self, page: u32) -> Result<Vec<YoutubeVideo>> {
        let Some(query) = &self.query else {
            bail!("No query string found")
        };
        
        Self::search_with_ytdlp(query, page).await
    }

    // GetSuggestions returns video suggestions based on prefix strings. This is the
    // same result as youtube search autocomplete.
    pub async fn get_suggestions(&self, prefix: &str) -> Result<Vec<YoutubeVideo>> {
        let url = format!(
            "http://suggestqueries.google.com/complete/search?client=firefox&ds=yt&q={prefix}"
        );
        let result = self.client.get(url).send().await?;
        match result.status() {
            StatusCode::OK => match result.text().await {
                Ok(text) => Self::parse_youtube_options(&text).ok_or_else(|| anyhow!("None Error")),
                Err(e) => bail!("Error during search: {}", e),
            },
            _ => bail!("Error during search"),
        }
    }

    // GetTrendingMusic fetch music trending based on region.
    // Region (ISO 3166 country code) can be provided in the argument.
    // Note: This still uses Invidious API as yt-dlp doesn't have a trending feature
    pub async fn get_trending_music(&self, region: &str) -> Result<Vec<YoutubeVideo>> {
        // Fallback to Invidious for trending music since yt-dlp doesn't support this
        let mut domains = vec![];
        
        if let Ok(domain_list) = Self::get_invidious_instance_list(&self.client).await {
            domains = domain_list;
        } else {
            for item in &INVIDIOUS_INSTANCE_LIST {
                domains.push((*item).to_string());
            }
        }

        domains.shuffle(&mut rand::rng());

        for domain in domains {
            let url = format!("{domain}/api/v1/trending?type=music&region={region}");
            
            if let Ok(result) = self.client.get(&url).send().await
                && result.status() == StatusCode::OK
                && let Ok(text) = result.text().await
                && let Some(videos) = Self::parse_youtube_options(&text)
            {
                return Ok(videos);
            }
        }
        
        bail!("Unable to fetch trending music from any Invidious instance")
    }

    fn parse_youtube_options(data: &str) -> Option<Vec<YoutubeVideo>> {
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            let mut vec: Vec<YoutubeVideo> = Vec::new();
            // below two lines are left for debug purpose
            // let mut file = std::fs::File::create("data.txt").expect("create failed");
            // file.write_all(data.as_bytes()).expect("write failed");
            if let Some(array) = value.as_array() {
                for v in array {
                    if let Some((title, video_id, length_seconds)) = Self::parse_youtube_item(v) {
                        vec.push(YoutubeVideo {
                            title,
                            length_seconds,
                            video_id,
                        });
                    }
                }
                return Some(vec);
            }
        }
        None
    }

    fn parse_youtube_item(value: &Value) -> Option<(String, String, u64)> {
        let title = value.get("title")?.as_str()?.to_owned();
        let video_id = value.get("videoId")?.as_str()?.to_owned();
        let length_seconds = value.get("lengthSeconds")?.as_u64()?;
        Some((title, video_id, length_seconds))
    }

    async fn get_invidious_instance_list(client: &Client) -> Result<Vec<String>> {
        let result = client.get(INVIDIOUS_DOMAINS).send().await?.text().await?;
        // Left here for debug
        // let mut file = std::fs::File::create("data.txt").expect("create failed");
        // file.write_all(result.as_bytes()).expect("write failed");
        if let Some(vec) = Self::parse_invidious_instance_list(&result) {
            return Ok(vec);
        }
        bail!("no instance list fetched")
    }

    fn parse_invidious_instance_list(data: &str) -> Option<Vec<String>> {
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            let mut vec = Vec::new();
            if let Some(array) = value.as_array() {
                for inner_value in array {
                    if let Some((uri, health)) = Self::parse_instance(inner_value)
                        && health > 95.0
                    {
                        vec.push(uri);
                    }
                }
            }
            if !vec.is_empty() {
                return Some(vec);
            }
        }
        None
    }

    fn parse_instance(value: &Value) -> Option<(String, f64)> {
        let obj = value.get(1)?.as_object()?;
        if obj.get("api")?.as_bool()? {
            let uri = obj.get("uri")?.as_str()?.to_owned();
            let health = obj
                .get("monitor")?
                .as_object()?
                .get("30dRatio")?
                .get("ratio")?
                .as_str()?
                .to_owned()
                .parse::<f64>()
                .ok();
            health.map(|health| (uri, health))
        } else {
            None
        }
    }
}
