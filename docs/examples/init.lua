-- Example user config: ~/.config/schemify/init.lua (docs/spec/config.md).
-- Runs at startup of `schemify` and `schemify-gui`; `--clean` skips it.

-- Options.
schemify.opt.snap = 5          -- snap step, schematic units
schemify.opt.grid = 20
schemify.opt.light = true      -- light canvas

-- Keymaps: a command name or a Lua function. Last wins over the default UI (D8).
schemify.keymap.set("Ctrl+Shift+N", "sim.netlist", { desc = "netlist the active cell" })
schemify.keymap.set("F5", function()
  schemify.cmd("sim.simulate")
  schemify.notify("simulating…")
end)

-- The simulator command line (never stored in a design file, D7).
schemify.sim("ngspice", {
  path = "ngspice",
  args = { "-b", "-r", "{raw}", "{netlist}" },
})

-- Event hooks.
schemify.on("saved", function(ev)
  schemify.notify("saved document " .. ev.doc)
end)

-- Packages: `schemify pack sync` installs them and writes schemify-pack.lock next to this file.
schemify.pack.add {
  {
    src = "https://github.com/someone/schemify-gmid-lut.git",
    version = "v1.2.0",
    grant = { "read_project", "spawn:ngspice" },
  },
  {
    url = "https://example.org/eye-diagram.wasm",
    sha256 = "0000000000000000000000000000000000000000000000000000000000000000",
    lazy = { cmd = "eye.open" },
  },
}

-- Split the config into modules under ~/.config/schemify/lua/:
-- require("keys")
