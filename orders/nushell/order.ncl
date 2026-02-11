# Nushell configuration
# https://www.nushell.sh/

let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    files = [
      {
        from_file = "nushell",
        prefix = ["~/Library/Application Support/"],
        when = { os = ["darwin"] },
      },
      {
        from_file = "nushell",
        prefix = ["~/.config/"],
        when = { os = ["linux"] },
      },
    ],
  },
} | Order
