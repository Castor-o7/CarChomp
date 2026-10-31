// One colour per track, by id, so a track keeps its colour between visits and
// on both the map and the track list. None is close to the orange live trail.
export const TRACK_COLORS = ['#2563eb', '#059669', '#db2777', '#7c3aed', '#0891b2', '#854d0e'];

export const trackColor = (id) => TRACK_COLORS[id % TRACK_COLORS.length];
