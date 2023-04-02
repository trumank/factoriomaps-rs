function center(area)
  return {(area.left_top.x + area.right_bottom.x) / 2, (area.left_top.y + area.right_bottom.y) / 2}
end

function chunk_key(chunk)
  return chunk.x .. ',' .. chunk.y
end

function map(tbl, func, ...)
  local new_tbl = {}
  for k, v in pairs(tbl) do new_tbl[k] = func(v, k, ...) end
  return new_tbl
end

function filter(tbl, func, ...)
  local new_tbl = {}
  local add = #tbl > 0
  for k, v in pairs(tbl) do
    if func(v, k, ...) then
      if add then
        table.insert(new_tbl, v)
      else
        new_tbl[k] = v
      end
    end
  end
  return new_tbl
end

function values(tbl)
  local new_tbl = {}
  for _, v in pairs(tbl) do
    table.insert(new_tbl, v)
  end
  return new_tbl
end

function get_neighbors(chunks, chunk)
  return filter(map({
    {x =  0, y = -1},
    {x =  0, y =  1},
    {x = -1, y =  0},
    {x =  1, y =  0},
  }, function(offset) return chunks[chunk_key{x = chunk.x + offset.x, y = chunk.y + offset.y}] end),
  function(neighbor) return neighbor ~= nil end)
end

function take_screenshots(player)
  local info = {}

  for name, surface in pairs(game.surfaces) do
    local chunks = {}

    -- initialize chunks and whether they contain player entities
    for chunk in surface.get_chunks() do
      -- player.print("x: " .. chunk.x .. ", y: " .. chunk.y)
      -- player.print("area: " .. serpent.line(chunk.area))

      local contains_entities = 0 < #surface.find_entities_filtered{area=chunk.area, force=player.force}
      local contains_tags = false
      for _, force in pairs(game.forces) do
        if 0 < #force.find_chart_tags(surface, chunk.area) then
          contains_tags = true
          break
        end
      end
      chunks[chunk_key(chunk)] = {
        x = chunk.x,
        y = chunk.y,
        distance = (contains_entities or contains_tags) and 0 or nil,
        contains_entities = contains_entities,
        contains_tags = contains_tags,
      }
    end

    -- calculate residual distances
    for i=1,5 do
      for _, chunk in pairs(chunks) do
        local min = nil
        for _, neigh in pairs(get_neighbors(chunks, chunk)) do
          -- print(serpent.line(min) .. ' ' .. serpent.line(neigh.distance))
          if min == nil or (neigh.distance ~= nil and min > neigh.distance) then
            min = neigh.distance
          end
        end
        if min ~= nil and (chunk.distance == nil or chunk.distance > min) then
          chunk.distance = min + 1
        end
      end
    end

    -- set flag if within distance of player entity
    for _, chunk in pairs(chunks) do
      if chunk.distance ~= nil and chunk.distance < 5 then
        chunk.within_distance = true
      else
        chunk.within_distance = false
      end
    end

    -- find and fill islands
    local queue = {}
    for key, chunk in pairs(chunks) do
      queue[key] = chunk
    end
    local key
    local edge_id = 0
    while true do
      local key, chunk = next(queue)
      if key == nil then
        break
      end
      if chunk.within_distance then
        chunk.edge = false
      else
        -- search area originating from chunk
        local visited = {}
        local to_visit = { [key] = chunk }
        local edge = false

        edge_id = edge_id + 1

        -- iterator neighbors until exhausted
        while true do
          local key, chunk = next(to_visit)

          -- all neighbors found
          if key == nil then
            for key, chunk in pairs(visited) do
              chunk.edge = edge
              chunk.edge_id = edge_id
              queue[key] = nil
            end
            break
          end

          local neighbors = get_neighbors(chunks, chunk)
          -- if less than 4 neighbors then edge of map found
          if #neighbors < 4 then
            edge = true
          end

          for _, chunk in pairs(neighbors) do
            local key = chunk_key(chunk)
            if not chunk.within_distance and not visited[key] then
              to_visit[key] = chunk
            end
          end

          -- remove current chunk from queue and add to visited
          visited[key] = chunk
          to_visit[key] = nil
        end

      end
      queue[key] = nil
    end

    -- build tags object
    local tags = {}
    for _, force in pairs(game.forces) do
       local f = map(force.find_chart_tags(surface), function(tag) return {
        position = tag.position,
        text = tag.text,
      } end)
      if 0 < #f then
        tags[force.name] = f
      end
    end

    -- write information about the current surface to a file
    local surface_info = {
      name = surface.name,
      tags = tags,
      chunks = map(filter(values(chunks), function(chunk) return not chunk.edge end), function(chunk) return {x = chunk.x, y = chunk.y} end),
    }

    -- omit surface entirely if there are no visible chunks
    if #surface_info.chunks > 0 then
      table.insert(info, surface_info)
    end
  end
  game.write_file('info.json', game.table_to_json(info))

  for i, surface_info in pairs(info) do
    local surface = game.surfaces[surface_info.name]

    surface.always_day = true

    -- create map tags
    if false then
      for _, tag in pairs(player.force.find_chart_tags(surface)) do
        tag.destroy()
      end

      for _, chunk in pairs(chunks) do
        local icon = 'signal-black'
        if chunk.edge == true then
          icon = 'signal-green'
        elseif chunk.edge == false then
          icon = 'signal-red'
        end
        -- local icon = 'signal-red'
        -- if chunk.contains_entities then
        --   icon = 'signal-green'
        -- elseif chunk.within_distance then
        --   icon = 'signal-black'
        -- end
        player.force.add_chart_tag(surface, {
          position = {chunk.x * 32 + 16, chunk.y * 32 + 16},
          -- text = key(chunk) .. ',' .. serpent.line(chunk.distance),
          text = tostring(chunk.edge_id or ''),
          icon = {type='virtual', name=icon}
        })
      end
      -- print(serpent.block(chunks))
    else
      for _, chunk in pairs(surface_info.chunks) do
        if not chunk.edge then
          game.take_screenshot({
            surface = surface,
            position = {chunk.x * 32 + 16, chunk.y * 32 + 16},
            resolution = {1024, 1024},
            zoom = 1,
            path = surface.name .. ',' .. chunk.x .. ',' .. chunk.y .. '.png',
            show_entity_info = true
          })
        end
      end
    end

    -- game.print(serpent.line(get_neighbors(chunks, {x = 0, y = 0})))
  end
end

script.on_event(defines.events.on_tick, function(event)
  game.set_wait_for_screenshots_to_finish()

  local player = game.connected_players[1]

  take_screenshots(player)

  game.print('screenshot finished')

  script.on_event(defines.events.on_tick, function(event) end)

  -- exit()
end)
