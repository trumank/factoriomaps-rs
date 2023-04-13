"use strict";

const chunkSize = 1 / 1024;
const tileSize = chunkSize / 32;

function createLayer(name, surface) {
  const mapInfoMap = new Map(surface.tiles.map(t => [`${t[0]},${t[1]},${t[2]}`, `tiles/${name}/${t[0]}/${t[1]}/${t[2]}.${mapInfo.extension}`]));

  const zooms = surface.tiles.map(t => t[0]);
  const minZoom = Math.min(...zooms);
  const maxZoom = Math.max(...zooms);
  const bounds = L.latLngBounds(surface.tiles
    .filter(([z]) => z == maxZoom)
    .flatMap(([_, x, y]) => [[x, y], [x + 1, y + 1]])
    .map(([x, y]) => [-y * chunkSize / 2, x * chunkSize / 2]));

  const tileLayer = new (L.TileLayer.extend({
    name,
    options: {
      minNativeZoom: minZoom,
      maxNativeZoom: maxZoom,
      minZoom: minZoom,
      maxZoom,
      bounds,
      noWrap: true,
      tileSize: 512,
      keepBuffer: 100,
    },
    getTileUrl: function(c) {
      return mapInfoMap.get(`${c.z},${c.x},${c.y}`) || '';
    },
    onAdd: function(map) {
      L.TileLayer.prototype.onAdd.call(this, map);
      map.fitBounds(this.options.bounds); // animate = false causes weird things to happen
    },
  }));

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

const layers = Object.entries(mapInfo.surfaces).map(
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
