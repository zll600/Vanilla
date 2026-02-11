# Sapling SCM configuration
# https://sapling-scm.com/

let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    files = [
      {
        from_file = "sapling.conf",
        prefix = ["~/Library/Preferences/sapling/"],
        when = { os = ["darwin"] },
      },
      {
        from_file = "sapling.conf",
        prefix = ["~/.config/sapling/"],
        when = { os = ["linux"] },
      },
    ],
  },
} | Order
