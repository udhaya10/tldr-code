const serialize = require('node-serialize');

export function handler(req, res, db) {
    const d = req.query.d;
    serialize.unserialize(d);
}
