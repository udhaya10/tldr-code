import * as serialize from 'node-serialize';

export function handler(req: any, res: any, db: any) {
    const d = req.query.d;
    serialize.unserialize(d);
}
