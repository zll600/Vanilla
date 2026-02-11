# Git configuration

let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/.config/git/"],
    files = [
      {
        from_file = "git",
        prefix = ["~/.config/"],
      },
      {
        from_file = "gitk",
        ignore = [
          "^set geometry",
        ],
      },
    ],
    when = { os = ["darwin", "linux"] },
  },
} | Order
