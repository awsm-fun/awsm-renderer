# Robot asset licenses

Everything in this directory that depicts a robot (`fixtures/`, `bundle/`,
`project/`, `golden.png`) is **derived from** two assets in the NVIDIA
Isaac Sim 5.0 asset library. NVIDIA's asset tables list each robot's own
license, which is the one that applies to it:

| robot | Isaac Sim asset | license |
|---|---|---|
| Franka Panda | `Isaac/Robots/FrankaRobotics/FrankaPanda/franka.usd` | Apache License 2.0 |
| ANYmal-D | `Isaac/Robots/ANYbotics/anymal_d/anymal_d.usd` | BSD 3-Clause (ANYbotics AG) |

Sources: [manipulator assets](https://docs.isaacsim.omniverse.nvidia.com/latest/assets/usd_assets_robots_manipulator.html),
[quadruped assets](https://docs.isaacsim.omniverse.nvidia.com/latest/assets/usd_assets_robots_quadruped.html).

**Changes made.** Each USD asset was converted with `awsm-renderer-isaac-export`
into a geometry-only glTF plus a `.mujoco.json` sidecar with its textures,
imported into the awsm-renderer scene editor, and exported as a player bundle.

"Franka", "Panda", "ANYmal" and "ANYbotics" are trademarks of their owners.
Their use here identifies the robots and implies no endorsement.

## Franka Panda: Apache License 2.0

Licensed under the Apache License, Version 2.0. The full text is in
`LICENSES/LICENSE-APACHE` in this repository, and at
<http://www.apache.org/licenses/LICENSE-2.0>.

## ANYmal-D: BSD 3-Clause

From [ANYbotics/anymal_d_simple_description](https://github.com/ANYbotics/anymal_d_simple_description/blob/master/LICENSE):

```text
Copyright 2023, ANYbotics AG.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in
   the documentation and/or other materials provided with the
   distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived
   from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
