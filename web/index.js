"use strict";

function createLayer(surface, tiles) {
  const mapInfoMap = new Map(tiles.map(t => [`${t[0]},${t[1]},${t[2]}`, `tiles/${surface}/${t[0]}/${t[1]}/${t[2]}.webp`]));

  return new (L.TileLayer.extend({
    options: {
      minNativeZoom: 12,
      maxNativeZoom: 20,
      minZoom: 12,
      maxZoom: 20,
      noWrap: true,
      tileSize: 512,
      keepBuffer: 100,
    },
    getTileUrl: function(c) {
      return mapInfoMap.get(`${c.z},${c.x},${c.y}`) || '';
    }
  }));
}

const layers = Object.fromEntries(Object.entries(mapInfo).map(([name, tiles]) => [name, createLayer(name, tiles)]));

const map = L.map('map', {
  center: [0, 0],
  zoom: 16,
  layers: [Object.values(layers)[0]],
  fadeAnimation: false,
  zoomAnimation: true,
  crs: L.CRS.Simple,
});

const layerControl = L.control.layers(layers).addTo(map);
