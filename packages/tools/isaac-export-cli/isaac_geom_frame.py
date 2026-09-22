"""Isaac Lab body poses -> the awsm-renderer pose-sink frame (reference code).

The sim side of docs/isaac.md "Streaming from Isaac Lab". Needs only numpy, so
it runs inside an Isaac Lab process unchanged:

    frame = GeomFrame("panda.mujoco.json", robot.data.body_names)
    pose = robot.data.body_link_pose_w[env_id].cpu().numpy()      # (B, 7), wxyz
    pose[:, :3] -= env.scene.env_origins[env_id].cpu().numpy()    # env-local
    payload = frame(pose)   # bytes: 7 x geom_count little-endian f32
    # send `payload` to the player, which hands it to apply_geom_poses

Verified offline against the isaac-robots fixtures (rest pose reproduced to
f32 precision with Isaac's body order shuffled); not yet run inside Isaac Lab.
"""

import json
import numpy as np

def qmul(a, b):
    """Hamilton product of [..., 4] quaternions stored w, x, y, z."""
    aw, ax, ay, az = np.moveaxis(a, -1, 0)
    bw, bx, by, bz = np.moveaxis(b, -1, 0)
    return np.stack([aw*bw - ax*bx - ay*by - az*bz,
                     aw*bx + ax*bw + ay*bz - az*by,
                     aw*by - ax*bz + ay*bw + az*bx,
                     aw*bz + ax*by - ay*bx + az*bw], axis=-1)

def qrot(q, v):
    """Rotate [..., 3] vectors by [..., 4] w, x, y, z quaternions."""
    w, u = q[..., :1], q[..., 1:]
    t = 2.0 * np.cross(u, v)
    return v + w * t + np.cross(u, t)

class GeomFrame:
    """Turns Isaac Lab body poses into the pose sink's geom frame."""

    def __init__(self, sidecar_path, isaac_body_names):
        sidecar = json.load(open(sidecar_path))
        geoms = sidecar["geoms"]
        names = [b.get("name") for b in sidecar["bodies"]]
        # Rest pose for every geom: bodies the sim does not report keep it.
        self.rest = np.array([g["world_pos"] + g["world_quat"] for g in geoms], np.float64)
        self.off_p = np.array([g["pos"] for g in geoms], np.float64)
        self.off_q = np.array([g["quat"] for g in geoms], np.float64)
        # Isaac Lab body index -> the geoms riding that body.
        self.rides = []
        for i, n in enumerate(isaac_body_names):
            if n in names:
                b = names.index(n)
                self.rides.append((i, np.array([k for k, g in enumerate(geoms) if g["body"] == b], int)))

    def __call__(self, body_pose_w):
        """body_pose_w: (B, 7) px py pz qw qx qy qz, one env, env-local."""
        out = self.rest.copy()
        for i, gs in self.rides:
            if len(gs) == 0:
                continue
            p, q = body_pose_w[i, :3], body_pose_w[i, 3:]
            out[gs, :3] = p + qrot(np.broadcast_to(q, (len(gs), 4)), self.off_p[gs])
            out[gs, 3:] = qmul(np.broadcast_to(q, (len(gs), 4)), self.off_q[gs])
        return out.astype("<f4").tobytes()  # 7 x geom_count little-endian f32
