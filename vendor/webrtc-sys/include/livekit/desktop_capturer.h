/*
 * Copyright 2025 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#pragma once
#include <memory>

#include "modules/desktop_capture/desktop_capturer.h"
#include "rust/cxx.h"

namespace livekit_ffi {
class DesktopFrame;
class DesktopCapturer;
class DesktopCapturerOptions;
class Source;
}  // namespace livekit_ffi

#include "webrtc-sys/src/desktop_capturer.rs.h"

namespace livekit_ffi {

// Concord: avisos do portal persistente (no .cpp).
class PortalRelay;

class DesktopCapturer : public webrtc::DesktopCapturer::Callback {
 public:
  // Concord: construtores e destrutor no .cpp, onde o PortalRelay é completo. O `relay` é
  // da captura do portal criada aqui.
  explicit DesktopCapturer(std::unique_ptr<webrtc::DesktopCapturer> capturer);
  DesktopCapturer(std::unique_ptr<PortalRelay> relay,
                  std::unique_ptr<webrtc::DesktopCapturer> capturer);
  ~DesktopCapturer() override;

  void OnCaptureResult(webrtc::DesktopCapturer::Result result,
                       std::unique_ptr<webrtc::DesktopFrame> frame) final;

  rust::Vec<Source> get_source_list() const;
  bool select_source(uint64_t id) const;
  void start(rust::Box<DesktopCapturerCallbackWrapper> callback);
  void capture_frame() const { capturer->CaptureFrame(); }

 private:
  // Concord: antes da captura, pra morrer depois dela.
  std::unique_ptr<PortalRelay> relay;
  std::unique_ptr<webrtc::DesktopCapturer> capturer;
  std::optional<rust::Box<DesktopCapturerCallbackWrapper>> callback;
};

class DesktopFrame {
 public:
  DesktopFrame(std::unique_ptr<webrtc::DesktopFrame> frame) : frame(std::move(frame)) {}
  int32_t width() const { return frame->size().width(); }

  int32_t height() const { return frame->size().height(); }

  int32_t left() const { return frame->rect().left(); }

  int32_t top() const { return frame->rect().top(); }

  int32_t stride() const { return frame->stride(); }

  const uint8_t* data() const { return frame->data(); }

 private:
  std::unique_ptr<webrtc::DesktopFrame> frame;
};

std::unique_ptr<DesktopCapturer> new_desktop_capturer(DesktopCapturerOptions options);

// Concord: token do portal por id de fonte (só o Linux usa).
void set_restore_token(uint64_t id, rust::Str token);
rust::String take_restore_token(uint64_t id);
}  // namespace livekit_ffi