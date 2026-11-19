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

#include "livekit/desktop_capturer.h"

#include "modules/desktop_capture/desktop_capture_options.h"
#ifdef WEBRTC_USE_PIPEWIRE
#include "modules/desktop_capture/linux/wayland/base_capturer_pipewire.h"
#endif

#include <unordered_map>

#include "rtc_base/synchronization/mutex.h"

using SourceList = webrtc::DesktopCapturer::SourceList;

namespace livekit_ffi {

// Concord: token do portal por id de fonte, nos dois sentidos. O RestoreTokenManager do
// libwebrtc não serve: o AddToken dele não sobrescreve e o GetToken não apaga.
namespace {
webrtc::Mutex tokens_mutex;
std::unordered_map<uint64_t, std::string> tokens;

void put_token(uint64_t id, std::string token) {
  webrtc::MutexLock lock(&tokens_mutex);
  tokens[id] = std::move(token);
}

std::string take_token(uint64_t id) {
  webrtc::MutexLock lock(&tokens_mutex);
  auto node = tokens.extract(id);
  return node.empty() ? std::string() : std::move(node.mapped());
}
}  // namespace

void set_restore_token(uint64_t id, rust::Str token) {
  put_token(id, std::string(token));
}

rust::String take_restore_token(uint64_t id) {
  return rust::String(take_token(id));
}

#ifdef WEBRTC_USE_PIPEWIRE
// Concord: o libwebrtc pede ao portal um token que só vale enquanto o processo vive
// (kTransient, fixo no BaseCapturerPipeWire::Start). Com o portal criado aqui ele vale até a
// pessoa revogar. O token vai no select_source e o novo volta pelo mesmo id.
class PortalRelay : public webrtc::ScreenCastPortal::PortalNotifier {
 public:
  webrtc::ScreenCastPortal* portal = nullptr;
  webrtc::BaseCapturerPipeWire* capturer = nullptr;
  uint64_t id = 0;
  // O que foi mandado: se o portal não deu outro, o RestoreToken() ainda é este, já gasto.
  std::string sent;

  void OnScreenCastRequestResult(webrtc::xdg_portal::RequestResponse result,
                                 uint32_t stream_node_id,
                                 int fd) override {
    std::string token = portal->RestoreToken();
    if (result == webrtc::xdg_portal::RequestResponse::kSuccess && id &&
        !token.empty() && token != sent) {
      put_token(id, token);
    }
    capturer->OnScreenCastRequestResult(result, stream_node_id, fd);
  }

  void OnScreenCastSessionClosed() override {
    capturer->OnScreenCastSessionClosed();
  }
};
#else
class PortalRelay {};
#endif

DesktopCapturer::DesktopCapturer(
    std::unique_ptr<webrtc::DesktopCapturer> capturer)
    : capturer(std::move(capturer)), callback(std::nullopt) {}

DesktopCapturer::DesktopCapturer(std::unique_ptr<PortalRelay> relay,
                                 std::unique_ptr<webrtc::DesktopCapturer> capturer)
    : relay(std::move(relay)),
      capturer(std::move(capturer)),
      callback(std::nullopt) {}

DesktopCapturer::~DesktopCapturer() = default;

bool DesktopCapturer::select_source(uint64_t id) const {
#ifdef WEBRTC_USE_PIPEWIRE
  if (relay) {
    relay->id = id;
    relay->sent = take_token(id);
    relay->portal->SetRestoreToken(relay->sent);
  }
#endif
  return capturer->SelectSource(id);
}

std::unique_ptr<DesktopCapturer> new_desktop_capturer(
    DesktopCapturerOptions options) {
  webrtc::DesktopCaptureOptions webrtc_options =
      webrtc::DesktopCaptureOptions::CreateDefault();
#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
  webrtc_options.set_allow_sck_capturer(true);
  webrtc_options.set_allow_sck_system_picker(options.allow_sck_system_picker);
#endif /* defined(WEBRTC_MAC) && !defined(WEBRTC_IOS) */
#ifdef _WIN64
  switch (options.source_type) {
    case SourceType::Screen:
      webrtc_options.set_allow_wgc_screen_capturer(true);
      break;
    case SourceType::Window:
      webrtc_options.set_allow_wgc_window_capturer(true);
      // https://github.com/webrtc-sdk/webrtc/blob/m137_release/modules/desktop_capture/desktop_capture_options.h#L133-L142
      webrtc_options.set_enumerate_current_process_windows(false);
      break;
    default:
      break;
  }
  webrtc_options.set_allow_directx_capturer(true);
#endif /* _WIN64 */
#ifdef WEBRTC_USE_PIPEWIRE
  webrtc_options.set_allow_pipewire(true);
#endif /* WEBRTC_USE_PIPEWIRE */

  // prefer_cursor_embedded indicate that the capturer should try to include the
  // cursor in the frame
  webrtc_options.set_prefer_cursor_embedded(options.include_cursor);

  std::unique_ptr<webrtc::DesktopCapturer> capturer = nullptr;
  switch (options.source_type) {
    case SourceType::Window:
      capturer = webrtc::DesktopCapturer::CreateWindowCapturer(webrtc_options);
      break;
    case SourceType::Screen:
      capturer = webrtc::DesktopCapturer::CreateScreenCapturer(webrtc_options);
      break;
    case SourceType::Generic:
#ifdef WEBRTC_USE_PIPEWIRE
      // Concord: o mesmo do CreateGenericCapturer no Wayland, com o portal persistente.
      if (webrtc_options.allow_pipewire() &&
          webrtc::DesktopCapturer::IsRunningUnderWayland()) {
        auto relay = std::make_unique<PortalRelay>();
        auto portal = std::make_unique<webrtc::ScreenCastPortal>(
            webrtc::CaptureType::kAnyScreenContent, relay.get(),
            webrtc_options.prefer_cursor_embedded());
        portal->SetPersistMode(
            webrtc::ScreenCastPortal::PersistMode::kPersistent);
        relay->portal = portal.get();
        auto pipewire = std::make_unique<webrtc::BaseCapturerPipeWire>(
            webrtc_options, std::move(portal));
        relay->capturer = pipewire.get();
        return std::make_unique<DesktopCapturer>(std::move(relay),
                                                 std::move(pipewire));
      }
#endif
      capturer = webrtc::DesktopCapturer::CreateGenericCapturer(webrtc_options);
      break;
    default:
      return nullptr;
  }

  if (!capturer) {
    return nullptr;
  }
  return std::make_unique<DesktopCapturer>(std::move(capturer));
}

void DesktopCapturer::start(
    rust::Box<DesktopCapturerCallbackWrapper> callback) {
  this->callback = std::move(callback);
  capturer->Start(this);
}

void DesktopCapturer::OnCaptureResult(
    webrtc::DesktopCapturer::Result result,
    std::unique_ptr<webrtc::DesktopFrame> frame) {
  CaptureResult ret_result = CaptureResult::ErrorPermanent;
  switch (result) {
    case webrtc::DesktopCapturer::Result::SUCCESS:
      ret_result = CaptureResult::Success;
      break;
    case webrtc::DesktopCapturer::Result::ERROR_PERMANENT:
      ret_result = CaptureResult::ErrorPermanent;
      break;
    case webrtc::DesktopCapturer::Result::ERROR_TEMPORARY:
      ret_result = CaptureResult::ErrorTemporary;
      break;
    default:
      break;
  }
  if (callback) {
    (*callback)->on_capture_result(
        ret_result, std::make_unique<DesktopFrame>(std::move(frame)));
  }
}

rust::Vec<Source> DesktopCapturer::get_source_list() const {
  SourceList list{};
  bool res = capturer->GetSourceList(&list);
  rust::Vec<Source> source_list{};
  if (res) {
    for (auto& source : list) {
      source_list.push_back(Source{static_cast<uint64_t>(source.id),
                                   source.title, source.display_id});
    }
  }
  return source_list;
}
}  // namespace livekit_ffi