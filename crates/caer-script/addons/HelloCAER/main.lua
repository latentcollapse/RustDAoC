-- HelloCAER — in-tree example addon (no retail assets).
-- Typed events via CAER.register; commands via CAER.command (queued intents only).

HelloCAERDB = HelloCAERDB or { swings = 0 }

CAER.print("HelloCAER loaded")

CAER.register("combat.swing", function(event)
  HelloCAERDB.swings = (HelloCAERDB.swings or 0) + 1
  CAER.print("HelloCAER swing " .. HelloCAERDB.swings .. " (" .. event.name .. ")")
end)
