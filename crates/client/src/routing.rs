use crate::{api, Client, Error};

#[derive(serde::Serialize)]
struct MoveRequest<'a> {
    app: &'a api::ChannelId,
    #[serde(rename = "appArgs")]
    arguments: String,
}

impl Client {
    pub async fn continue_in_dialplan(
        &self,
        channel: &api::ChannelId,
        context: Option<&str>,
        extension: Option<&str>,
        priority: Option<u16>,
    ) -> Result<(), Error> {
        if context
            .into_iter()
            .chain(extension)
            .any(|value| value.len() > 128 || value.chars().any(char::is_control))
        {
            return Err(Error::Invalid("invalid dialplan location"));
        }
        let priority = priority.map(|value| value.to_string());
        let query: Vec<(&str, &str)> = [
            ("context", context),
            ("extension", extension),
            ("priority", priority.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
        self.request(
            "channels.continue",
            &[("channelId", channel.as_str())],
            &query,
        )
        .await
    }

    pub async fn move_channel(
        &self,
        channel: &api::ChannelId,
        application: &api::ChannelId,
        arguments: &[String],
    ) -> Result<(), Error> {
        if arguments.len() > 32
            || arguments.iter().any(|argument| {
                argument.is_empty()
                    || argument.len() > 256
                    || argument.contains(',')
                    || argument.chars().any(char::is_control)
            })
        {
            return Err(Error::Invalid("invalid Stasis arguments"));
        }
        let arguments = arguments.join(",");
        if arguments.len() > 4096 {
            return Err(Error::Capacity);
        }
        self.request_body(
            "channels.move",
            &[("channelId", channel.as_str())],
            &[],
            &MoveRequest {
                app: application,
                arguments,
            },
        )
        .await
    }
}
