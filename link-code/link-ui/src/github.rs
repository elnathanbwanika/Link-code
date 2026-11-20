//! Minimal GitHub REST client used by the Git panel.
//!
//! Only two endpoints are needed: the authenticated user (to prove a token
//! works and to label the account) and the repository list. Requests respect
//! the app-wide proxy because they are built through
//! [`link_models::proxy::apply`], exactly like the model providers.

use serde::Deserialize;

/// Secret-store key holding the personal access token.
pub const GITHUB_TOKEN_KEY: &str = "github-token";

const API: &str = "https://api.github.com";
const USER_AGENT: &str = "Link-Code";

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubUser {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl GitHubUser {
    /// Best display name for the connected account.
    pub fn display(&self) -> String {
        match self.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => format!("{name} ({})", self.login),
            _ => self.login.clone(),
        }
    }

    /// Commit identity derived from the account.
    pub fn commit_name(&self) -> String {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.login)
            .to_string()
    }

    pub fn commit_email(&self) -> String {
        format!("{}@users.noreply.github.com", self.login)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRepo {
    pub full_name: String,
    pub name: String,
    pub private: bool,
    pub clone_url: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn client() -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30));
    link_models::proxy::apply(builder)
        .build()
        .map_err(|error| format!("could not build the HTTP client: {error}"))
}

/// Turn a non-success response into a sentence a user can act on.
async fn api_error(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();
    match status.as_u16() {
        401 => "GitHub rejected the token (401). Check that it has not expired.".to_string(),
        403 => {
            if body.contains("rate limit") {
                "GitHub rate limit reached. Try again in a few minutes.".to_string()
            } else {
                "GitHub refused the request (403). The token may lack the `repo` scope.".to_string()
            }
        }
        404 => "GitHub returned 404. The token may not have access to this resource.".to_string(),
        _ => {
            if body.is_empty() {
                format!("GitHub returned HTTP {status}")
            } else {
                format!("GitHub returned HTTP {status}: {body}")
            }
        }
    }
}

/// Verify a token and return the account behind it.
pub async fn get_user(token: &str) -> Result<GitHubUser, String> {
    let response = client()?
        .get(format!("{API}/user"))
        .bearer_auth(token.trim())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("could not reach GitHub: {error}"))?;
    if !response.status().is_success() {
        return Err(api_error(response).await);
    }
    response
        .json::<GitHubUser>()
        .await
        .map_err(|error| format!("GitHub sent an unexpected reply: {error}"))
}

/// Every repository the authenticated user can push to, newest first.
///
/// The endpoint is paginated at 100 per page; a few hundred repositories is
/// normal for an organisation member, so the loop follows `Link` headers up to
/// a sane cap instead of silently showing the first page only.
pub async fn list_repos(token: &str) -> Result<Vec<GitHubRepo>, String> {
    let client = client()?;
    let mut repos: Vec<GitHubRepo> = Vec::new();
    let mut page = 1u32;
    loop {
        let response = client
            .get(format!("{API}/user/repos"))
            .bearer_auth(token.trim())
            .header("Accept", "application/vnd.github+json")
            .query(&[
                ("per_page", "100".to_string()),
                ("page", page.to_string()),
                ("sort", "updated".to_string()),
                (
                    "affiliation",
                    "owner,collaborator,organization_member".to_string(),
                ),
            ])
            .send()
            .await
            .map_err(|error| format!("could not reach GitHub: {error}"))?;
        if !response.status().is_success() {
            return Err(api_error(response).await);
        }
        let batch = response
            .json::<Vec<GitHubRepo>>()
            .await
            .map_err(|error| format!("GitHub sent an unexpected repository list: {error}"))?;
        let received = batch.len();
        repos.extend(batch);
        if received < 100 || page >= 20 {
            break;
        }
        page += 1;
    }
    Ok(repos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_identity_falls_back_to_the_login() {
        let user = GitHubUser {
            login: "octocat".to_string(),
            name: Some("  ".to_string()),
        };
        assert_eq!(user.commit_name(), "octocat");
        assert_eq!(user.commit_email(), "octocat@users.noreply.github.com");
        assert_eq!(user.display(), "octocat");
    }

    #[test]
    fn commit_identity_prefers_the_profile_name() {
        let user = GitHubUser {
            login: "octocat".to_string(),
            name: Some("The Octocat".to_string()),
        };
        assert_eq!(user.commit_name(), "The Octocat");
        assert_eq!(user.display(), "The Octocat (octocat)");
    }
}
