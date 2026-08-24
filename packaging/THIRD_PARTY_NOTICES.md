# Third-party notices

OpenFlow is licensed under Apache-2.0. Production installers statically link
the following independently licensed inference runtimes:

- `whisper.cpp`, Copyright (c) 2023-2026 Georgi Gerganov, MIT License.
- `llama.cpp`, Copyright (c) 2023-2026 Georgi Gerganov, MIT License.

The model files are not redistributed in an OpenFlow installer. They are
downloaded only after a user selects a model, retain their upstream licenses,
and are stored in the server's model cache. The model catalog shows the license
before download. In particular, Qwen3 model weights are offered under
Apache-2.0 and the Whisper model files retain their upstream terms.

The full source and license text for the linked projects are available from:

- https://github.com/ggml-org/whisper.cpp
- https://github.com/ggml-org/llama.cpp
- https://huggingface.co/Qwen

This notice does not replace the license files supplied by those projects.
Verified package builds also generate `THIRD_PARTY_LICENSES.txt`, containing the
locked Cargo, production npm, and linked native dependency inventory together
with the corresponding license texts. Installer creation fails if that
aggregate cannot be generated or verified.

## MIT License used by whisper.cpp and llama.cpp

Copyright (c) 2023-2026 Georgi Gerganov and contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
