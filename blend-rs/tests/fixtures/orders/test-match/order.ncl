let metadata = import "blend://metadata" in
{
  blend = {
    prefix = ["~/.config/test-match/"],
    files = [
      {
        name = "config.toml",
        from_config = {
          # Platform-specific font size
          font_size = metadata.os |> match {
            "darwin" => 14,
            "linux" => 12,
            _ => 10,
          },
          # Static value
          theme = "catppuccin",
        },
      },
    ],
  },
}
