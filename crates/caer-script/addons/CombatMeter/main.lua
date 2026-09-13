-- CombatMeter — in-tree example addon (no retail assets).
-- Typed combat events via CAER.register are read-only projections.
-- CAER.command queues AddonIntent only; this addon never sees WorldState.

CombatMeterDB = CombatMeterDB or {
  swings = 0,
  damage = 0,
  deaths = 0,
  revives = 0,
}

CAER.print("CombatMeter loaded")

local function number_or_zero(v)
  if type(v) == "number" then
    return v
  end
  return 0
end

CAER.register("combat.swing", function(event)
  CombatMeterDB.swings = (CombatMeterDB.swings or 0) + 1
  local dmg = number_or_zero(event.damage)
  CombatMeterDB.damage = (CombatMeterDB.damage or 0) + dmg
  CAER.print(
    "CombatMeter swing " .. CombatMeterDB.swings
      .. " damage=" .. tostring(dmg)
      .. " total=" .. tostring(CombatMeterDB.damage)
      .. " result=" .. tostring(event.result or "")
      .. " hp_pct=" .. tostring(event.hp_pct or "")
      .. " (" .. event.name .. ")"
  )
end)

CAER.register("combat.player_died", function(event)
  CombatMeterDB.deaths = (CombatMeterDB.deaths or 0) + 1
  CAER.print("CombatMeter death " .. CombatMeterDB.deaths .. " (" .. event.name .. ")")
end)

CAER.register("combat.player_revived", function(event)
  CombatMeterDB.revives = (CombatMeterDB.revives or 0) + 1
  CAER.print("CombatMeter revive " .. CombatMeterDB.revives .. " (" .. event.name .. ")")
end)

-- `/cm` in a chat projection queues a say intent. The host does not apply WorldState here.
CAER.register("chat.message", function(event)
  local text = event.text or ""
  if text == "/cm" then
    local line = "CombatMeter swings=" .. tostring(CombatMeterDB.swings or 0)
      .. " damage=" .. tostring(CombatMeterDB.damage or 0)
      .. " deaths=" .. tostring(CombatMeterDB.deaths or 0)
    CAER.print(line)
    CAER.command("chat.say", { text = line })
  end
end)
