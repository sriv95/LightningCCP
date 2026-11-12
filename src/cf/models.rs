use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct User {
    pub handle: String,
    pub rating: Option<i64>,
    #[serde(rename = "maxRating")]
    pub max_rating: Option<i64>,
    pub rank: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Problem {
    #[serde(rename = "contestId")]
    pub contest_id: i64,
    pub index: String,
    pub name: String,
    pub rating: Option<i64>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Problem {
    pub fn key(&self) -> String {
        format!("{}{}", self.contest_id, self.index)
    }

    pub fn url(&self) -> String {
        format!("https://codeforces.com/problemset/problem/{}/{}", self.contest_id, self.index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Contest {
    pub id: i64,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub phase: String,
    #[serde(rename = "startTimeSeconds")]
    pub start_time: Option<i64>,
    #[serde(rename = "durationSeconds")]
    pub duration: i64,
}

impl Contest {
    pub fn url(&self) -> String {
        format!("https://codeforces.com/contest/{}", self.id)
    }
}

/// `rating` is the rating after the contest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RatingChange {
    #[serde(rename = "ratingUpdateTimeSeconds")]
    pub time: i64,
    #[serde(rename = "newRating")]
    pub rating: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub rank: i64,
    pub handles: Vec<String>,
    pub points: f64,
    pub penalty: i64,
}

/// One member's contest result, from a standings row or from `contest.status`. `last_ac` is contest-relative seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct ContestResult {
    pub handle: String,
    pub points: f64,
    pub penalty: i64,
    pub solved: i64,
    pub last_ac: Option<i64>,
    pub participant_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub id: i64,
    pub created: i64,
    pub problem_key: String,
    pub verdict: Option<String>,
}
