# Alacritty terminal configuration (Linux)
# https://alacritty.org/config-alacritty.html
let metadata = import "blend://metadata" in
let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/.config/alacritty/"],
    files = [
      {
        name = "alacritty.toml",
        from_config =
          {
            window = {
              decorations =
                metadata.os
                |> match {
                  "darwin" => "Buttonless",
                  _ => "None",
                },
              opacity = 0.7,
            },

            font = {
              size = 12.0,
              bold = { family = "JetBrains Mono", style = "Bold" },
              bold_italic = { family = "Jetbrains Mono", style = "Bold Italic" },
              italic = { family = "Jetbrains Mono", style = "Italic" },
              normal = { family = "Jetbrains Mono", style = "Regular" },
            },
          }
          & (
            metadata.os
            |> match {
              "linux" =>
                {
                  terminal.shell = {
                    program = "/usr/bin/env",
                    args = ["elvish"],
                  },
                },
              _ => {},
            }
          ),
      },
    ],
  },
} | Order
