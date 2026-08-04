# mozangle 0.6.0 and bundled ANGLE notices

This notice bundle covers the C and C++ sources compiled into the Windows
`libEGL.dll` and `libGLESv2.dll` artifacts shipped with Pliego.

Source identity:

- crates.io package: `mozangle` 0.6.0
- crates.io checksum: `60b428c032f0af701a3ca440c92e7b552c25b2a5af2b08a85077ee8e9d2ae699`
- mozangle source revision: `7be30d4be68583169ced927e7b5dab7cca6f185f`
- vendored Mozilla source: `FIREFOX_140_12_0esr_RELEASE`, revision
  `f8025617e815f21388b40baf189338d31a5f9a0a`
- bundled ANGLE checkout marker: `6eb59c58d21b`

The full mozangle and ANGLE BSD-3-Clause texts are distributed beside this file
as `LICENSE` and `ANGLE_LICENSE`.

## Chromium-derived sources

The compiled source set contains Chromium-derived SHA-1, checked arithmetic,
cache, tracing, and zlib utility code. The vendored files retain these notices:

```text
Copyright (c) 2011 The Chromium Authors. All rights reserved.
Copyright 2013 The Chromium Authors. All rights reserved.
Copyright 2014 The Chromium Authors. All rights reserved.
Copyright 2017 The Chromium Authors. All rights reserved.
Copyright 2018 The Chromium Authors. All rights reserved.
Copyright 2019 The Chromium Authors.
```

Those files refer to the BSD-style terms reproduced in `ANGLE_LICENSE`.

## Apple SystemInfo

```text
Copyright (C) 2009 Apple Inc. All Rights Reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:
1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY APPLE INC. ``AS IS'' AND ANY
EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL APPLE INC. OR
CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY
OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## xxHash

```text
xxHash - Fast Hash algorithm
Copyright (C) 2012-2016, Yann Collet

BSD 2-Clause License

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice,
  this list of conditions and the following disclaimer.
* Redistributions in binary form must reproduce the above copyright notice,
  this list of conditions and the following disclaimer in the documentation
  and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
```

## volk

```text
Copyright (c) 2018-2019 Arseny Kapoulkine

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## MurmurHash3 / PMurHash

```text
MurmurHash3 was written by Austin Appleby, and is placed in the public domain.
This implementation was written by Shane Day, and is also public domain.
```

## Khronos and Vulkan headers

The compiled source set includes Khronos EGL, GLES, KHR, WGL, and Vulkan
headers, with copyright dates from 2007 through 2022. Their vendored headers
identify `MIT`, `Apache-2.0`, or `Apache-2.0 OR MIT`. Vulkan header notices also
name Valve Corporation and LunarG, Inc. The complete MIT and Apache-2.0 texts
are included in Pliego's generated third-party license report.

The compiled Vulkan path also contains:

```text
Copyright 2020 The SwiftShader Authors. All Rights Reserved.
Licensed under the Apache License, Version 2.0.
```

## GNU Bison generated parser skeletons

Two compiled ANGLE parser files were generated by GNU Bison 3.8.2 and contain
this exception:

```text
As a special exception, you may create a larger work that contains part or all
of the Bison parser skeleton and distribute that work under terms of your
choice, so long as that work isn't itself a parser generator using the skeleton
or a modified version thereof as a parser skeleton. Alternatively, if you
modify or redistribute the parser skeleton itself, you may (at your option)
remove this special exception, which will cause the skeleton and the resulting
Bison output files to be licensed under the GNU General Public License without
this special exception.

This special exception was added by the Free Software Foundation in version
2.2 of Bison.
```

Pliego distributes the resulting rendering libraries, not a parser generator
or the Bison skeleton source.

## zlib

ANGLE links zlib through the Rust `libz-sys` dependency. Its complete zlib
license and package attribution are included in Pliego's generated third-party
license report.
