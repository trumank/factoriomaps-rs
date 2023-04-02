"use strict";

function createLayer(name, surface) {
  const mapInfoMap = new Map(surface.tiles.map(t => [`${t[0]},${t[1]},${t[2]}`, `tiles/${name}/${t[0]}/${t[1]}/${t[2]}.webp`]));

  let minZoom = Number.POSITIVE_INFINITY;
  let maxZoom = Number.NEGATIVE_INFINITY;
  for (const tile of surface.tiles) {
    minZoom = Math.min(minZoom, tile[0]);
    maxZoom = Math.max(maxZoom, tile[0]);
  }

  const tileLayer = new (L.TileLayer.extend({
    options: {
      minNativeZoom: minZoom,
      maxNativeZoom: maxZoom,
      minZoom,
      maxZoom,
      noWrap: true,
      tileSize: 512,
      keepBuffer: 100,
    },
    getTileUrl: function(c) {
      return mapInfoMap.get(`${c.z},${c.x},${c.y}`) || '';
    }
  }));

  const chunkSize = 1 / 1024;
  const tileSize = chunkSize / 32;

  const markers = Object.values(surface.tags).flat()
    .map(tag => {
      const marker = L.marker([tileSize * -tag.position.y, tileSize * tag.position.x])
      if (tag.text) marker.bindPopup(tag.text);
      return marker;
    });

  return {
    group: new L.LayerGroup([tileLayer]),
    tiles: tileLayer,
    markers: L.layerGroup(markers),
  };
}

const layers = Object.entries(mapInfo).map(
  ([name, surface]) => [name, createLayer(name, surface)]
);

const map = L.map('map', {
  center: [0, 0],
  zoom: 16,
  layers: [layers[0][1].group],
  fadeAnimation: false,
  zoomAnimation: true,
  crs: L.CRS.Simple,
});

const tagsLayer = new (L.Layer.extend({
  onAdd: function(_) {
    for (const [_, layer] of layers) {
      layer.markers.addTo(layer.group);
    }
  },
  onRemove: function(_) {
    for (const [_, layer] of layers) {
      layer.markers.removeFrom(layer.group);
    }
  },
}));

const layerControl = L.control.layers(
  Object.fromEntries(layers.map(([name, surface]) => [name, surface.group])),
  {tags: tagsLayer},
).addTo(map);
