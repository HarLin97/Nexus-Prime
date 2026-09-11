import { describe, expect, it } from "vitest";
import type { DeviceConfig } from "../types";
import { applyImePresetConfig, IME_PRESETS } from "./imePreset";

function configWithLegacyVoiceGestures(): DeviceConfig {
  return {
    button_aliases: {},
    button_bindings: {
      mic: { type: "ComboKey", value: [0xa2, 0x5b] },
      voice: { type: "ComboKey", value: [0xa2, 0x5b] },
    },
    long_press_bindings: {
      mic: { type: "SingleKey", value: 0xa5 },
      menu: { type: "SingleKey", value: 0x20 },
    },
    multi_click_bindings: {
      voice: { 2: { type: "SingleKey", value: 0xa5 } },
      menu: { 2: { type: "SingleKey", value: 0x41 } },
    },
    voice_hotkey: ["leftctrl", "leftwin"],
    trigger_mode: "Hold",
    bluetooth_address: null,
  };
}

describe("applyImePresetConfig", () => {
  it("configures the ChatGPT activation preset with hold-to-talk", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "codex");

    expect(next).toMatchObject({
      button_bindings: {
        home: { type: "FocusChatGpt", value: null },
        mic: { type: "ComboKey", value: [0xa2, 0xa0, 0x44] },
        voice: { type: "ComboKey", value: [0xa2, 0xa0, 0x44] },
      },
      voice_hotkey: ["leftctrl", "leftshift", "d"],
      voice_input_profile: "codex",
      voice_shortcut_enabled: true,
      trigger_mode: "Hold",
    });
  });

  it("keeps the Home binding untouched for non-ChatGPT presets", () => {
    const original = configWithLegacyVoiceGestures();
    original.button_bindings.home = { type: "ComboKey", value: [0x5b, 0x44] };

    const next = applyImePresetConfig(original, "qianwen");

    expect(next.button_bindings.home).toEqual({ type: "ComboKey", value: [0x5b, 0x44] });
  });

  it("exposes exactly WeChat's two official voice modes", () => {
    expect(
      Object.keys(IME_PRESETS).filter((profile) => profile.startsWith("wechat")),
    ).toEqual(["wechat", "wechat-current"]);
  });

  it("configures WeChat start-voice as a Ctrl+Win click pulse", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "wechat");

    expect(next).toMatchObject({
      button_bindings: {
        mic: { type: "ComboKey", value: [0xa2, 0x5b] },
        voice: { type: "ComboKey", value: [0xa2, 0x5b] },
      },
      voice_hotkey: ["leftctrl", "leftwin"],
      voice_input_profile: "wechat",
      voice_shortcut_enabled: true,
      trigger_mode: "Toggle",
      long_press_bindings: { menu: { type: "SingleKey", value: 0x20 } },
      multi_click_bindings: { menu: { 2: { type: "SingleKey", value: 0x41 } } },
    });
  });

  it("configures the current WeChat hold-to-talk shortcut and removes legacy voice gestures", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "wechat-current");

    expect(next).toMatchObject({
      button_bindings: {
        mic: { type: "ComboKey", value: [0xa2, 0xa0, 0x44] },
        voice: { type: "ComboKey", value: [0xa2, 0xa0, 0x44] },
      },
      voice_hotkey: ["leftctrl", "leftshift", "d"],
      voice_input_profile: "wechat-current",
      voice_shortcut_enabled: true,
      trigger_mode: "Hold",
      long_press_bindings: { menu: { type: "SingleKey", value: 0x20 } },
      multi_click_bindings: { menu: { 2: { type: "SingleKey", value: 0x41 } } },
    });
  });

  it("configures the Doubao hold shortcut and removes legacy voice gestures", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "doubao-hold");

    expect(next).toMatchObject({
      button_bindings: {
        mic: { type: "SingleKey", value: 0xa5 },
        voice: { type: "SingleKey", value: 0xa5 },
      },
      voice_hotkey: ["rightalt"],
      voice_input_profile: "doubao-hold",
      voice_shortcut_enabled: true,
      trigger_mode: "Hold",
      long_press_bindings: { menu: { type: "SingleKey", value: 0x20 } },
      multi_click_bindings: { menu: { 2: { type: "SingleKey", value: 0x41 } } },
    });
  });

  it("configures Qianwen with the direct right-Alt voice profile", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "qianwen");

    expect(next).toMatchObject({
      button_bindings: {
        mic: { type: "SingleKey", value: 0xa5 },
        voice: { type: "SingleKey", value: 0xa5 },
      },
      voice_hotkey: ["rightalt"],
      voice_input_profile: "qianwen",
      voice_shortcut_enabled: true,
      trigger_mode: "Hold",
    });
  });

  it.each([
    ["qianwen-left-ctrl", [0xa2], ["leftctrl"]],
    ["qianwen-left-ctrl-win", [0xa2, 0x5b], ["leftctrl", "leftwin"]],
    ["qianwen-left-win-alt", [0x5b, 0xa4], ["leftwin", "leftalt"]],
    ["qianwen", [0xa5], ["rightalt"]],
  ] as const)("configures %s as a Qianwen hold shortcut", (preset, shortcutVks, voiceHotkey) => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), preset);

    expect(IME_PRESETS[preset]).toMatchObject({ shortcutVks, voiceHotkey, triggerMode: "Hold" });
    expect(next).toMatchObject({
      button_bindings: {
        mic: shortcutVks.length === 1
          ? { type: "SingleKey", value: shortcutVks[0] }
          : { type: "ComboKey", value: shortcutVks },
        voice: shortcutVks.length === 1
          ? { type: "SingleKey", value: shortcutVks[0] }
          : { type: "ComboKey", value: shortcutVks },
      },
      voice_hotkey: voiceHotkey,
      voice_input_profile: preset,
      voice_shortcut_enabled: true,
      trigger_mode: "Hold",
    });
  });

  it("configures the Doubao hands-free shortcut as a click-mode Alt+Space chord", () => {
    const next = applyImePresetConfig(configWithLegacyVoiceGestures(), "doubao-hands-free");

    expect(next).toMatchObject({
      button_bindings: {
        mic: { type: "ComboKey", value: [0xa5, 0x20] },
        voice: { type: "ComboKey", value: [0xa5, 0x20] },
      },
      voice_hotkey: ["rightalt", "space"],
      voice_input_profile: "doubao-hands-free",
      voice_shortcut_enabled: true,
      trigger_mode: "Toggle",
      long_press_bindings: { menu: { type: "SingleKey", value: 0x20 } },
      multi_click_bindings: { menu: { 2: { type: "SingleKey", value: 0x41 } } },
    });
  });
});
