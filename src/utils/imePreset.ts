import type { DeviceConfig, KeyAction, TriggerMode, VoiceInputProfile } from "../types";
import { normalizeVoiceShortcutConfig } from "./voiceShortcut";

export type ImePreset = VoiceInputProfile;

export interface ImePresetDefinition {
  shortcutVks: number[];
  voiceHotkey: string[];
  triggerMode: TriggerMode;
  applyHint: string;
  logMessage: string;
}

export const IME_PRESETS: Record<ImePreset, ImePresetDefinition> = {
  codex: {
    shortcutVks: [0xa2, 0xa0, 0x44],
    voiceHotkey: ["leftctrl", "leftshift", "d"],
    triggerMode: "Hold",
    applyHint: "已应用：主页键打开并聚焦 ChatGPT；语音键 = 左 Ctrl + 左 Shift + D，按住",
    logMessage: "设置建议：已应用 ChatGPT / Codex 预设（主页键聚焦输入框，语音键按住听写）",
  },
  wechat: {
    shortcutVks: [0xa2, 0x5b],
    voiceHotkey: ["leftctrl", "leftwin"],
    triggerMode: "Toggle",
    applyHint: "已应用：微信输入法「启动语音输入」= 左 Ctrl + 左 Win",
    logMessage: "设置建议：已应用微信输入法「启动语音输入」（左 Ctrl + 左 Win，点击触发）",
  },
  "wechat-current": {
    shortcutVks: [0xa2, 0xa0, 0x44],
    voiceHotkey: ["leftctrl", "leftshift", "d"],
    triggerMode: "Hold",
    applyHint: "已应用：微信输入法「按住说话」= 左 Ctrl + 左 Shift + D",
    logMessage: "设置建议：已应用微信输入法「按住说话」（左 Ctrl + 左 Shift + D，按住开始、松手结束）",
  },
  qianwen: {
    shortcutVks: [0xa5],
    voiceHotkey: ["rightalt"],
    triggerMode: "Hold",
    applyHint: "已应用：千问语音键 = 右 Alt，触发模式 = 按住",
    logMessage: "设置建议：已应用千问按住说话映射（右 Alt）",
  },
  "qianwen-left-ctrl": {
    shortcutVks: [0xa2],
    voiceHotkey: ["leftctrl"],
    triggerMode: "Hold",
    applyHint: "已应用：千问语音键 = 左 Ctrl，触发模式 = 按住",
    logMessage: "设置建议：已应用千问按住说话映射（左 Ctrl）",
  },
  "qianwen-left-ctrl-win": {
    shortcutVks: [0xa2, 0x5b],
    voiceHotkey: ["leftctrl", "leftwin"],
    triggerMode: "Hold",
    applyHint: "已应用：千问语音键 = 左 Ctrl + 左 Win，触发模式 = 按住",
    logMessage: "设置建议：已应用千问按住说话映射（左 Ctrl + 左 Win）",
  },
  "qianwen-left-win-alt": {
    shortcutVks: [0x5b, 0xa4],
    voiceHotkey: ["leftwin", "leftalt"],
    triggerMode: "Hold",
    applyHint: "已应用：千问语音键 = 左 Win + 左 Alt，触发模式 = 按住",
    logMessage: "设置建议：已应用千问按住说话映射（左 Win + 左 Alt）",
  },
  "doubao-hold": {
    shortcutVks: [0xa5],
    voiceHotkey: ["rightalt"],
    triggerMode: "Hold",
    applyHint: "已应用：豆包长按模式，语音键 = 右 Alt，触发模式 = 按住",
    logMessage: "设置建议：已快速应用豆包长按语音映射（右 Alt）",
  },
  "doubao-hands-free": {
    shortcutVks: [0xa5, 0x20],
    voiceHotkey: ["rightalt", "space"],
    triggerMode: "Toggle",
    applyHint: "已应用：豆包免按模式，语音键 = 右 Alt + 空格，触发模式 = 点击",
    logMessage: "设置建议：已快速应用豆包免按语音映射（右 Alt + 空格）",
  },
};

function shortcutAction(shortcutVks: readonly number[]): KeyAction {
  if (shortcutVks.length === 1) {
    return { type: "SingleKey", value: shortcutVks[0] };
  }
  return { type: "ComboKey", value: [...shortcutVks] };
}

/** Build a complete, dedicated voice-key configuration for an input-method preset. */
export function applyImePresetConfig(config: DeviceConfig, preset: ImePreset): DeviceConfig {
  const definition = IME_PRESETS[preset];
  const action = shortcutAction(definition.shortcutVks);
  const buttonBindings: Record<string, KeyAction> = {
    ...config.button_bindings,
    mic: action,
    voice: action,
  };
  if (preset === "codex") {
    buttonBindings.home = { type: "FocusChatGpt", value: null };
  }

  return normalizeVoiceShortcutConfig({
    ...config,
    button_bindings: buttonBindings,
    voice_hotkey: [...definition.voiceHotkey],
    voice_shortcut_enabled: true,
    trigger_mode: definition.triggerMode,
    voice_input_profile: preset,
  });
}
