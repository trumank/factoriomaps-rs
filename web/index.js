"use strict";
const mapInfoMap = new Map(mapInfo.map(t => [`${t[0]},${t[1]},${t[2]}`, `tiles/nauvis/${t[0]}/${t[1]}/${t[2]}.webp`]));

let map = L.map('map', {
  center: [0, 0],
  zoom: 16,
  layers: [],
  fadeAnimation: false,
  zoomAnimation: true,
  crs: L.CRS.Simple,
});

L.TileLayer.Factorio = L.TileLayer.extend({
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
});

L.tileLayer.factorio = function() {
  return new L.TileLayer.Factorio();
}

L.tileLayer.factorio().addTo(map);

