package com.today.diary;

import android.content.Context;
import android.view.View;
import android.view.inputmethod.EditorInfo;

import com.google.androidgamesdk.GameActivity;
import com.google.androidgamesdk.gametextinput.Settings;

/**
 * GameActivity 的壳，只为了把输入法上屏的字与退格接住。
 *
 * <p>背景：winit 0.30.13 的 Android 后端不消费 game-activity 的 {@code TextEvent}，
 * 而软键盘敲出来的字（中文候选、英文单词）只走那条路——不接这一手，手机上
 * 一个字都输不进去（真机验证过：键盘弹得出来、候选也有，就是进不了输入框）。
 *
 * <p>钩子在 {@code createSurfaceView()}：GameActivity 把输入法连接挂在它自己的
 * {@code InputEnabledSurfaceView} 上（Activity 本身没有 {@code onCreateInputConnection}），
 * 所以换一个自己的 SurfaceView，交出去一份带桥的 {@code gametextinput.InputConnection}：
 * {@code commitText} 推给 native，{@code deleteSurroundingText} 退格也给一份。
 *
 * <p>两份都照旧调 {@code super}：GameActivity 自己那份 GameTextInput 状态得跟输入法
 * 保持一致（否则输入法对光标与周围文本的假设会错位）。代价是万一哪天 winit 开始转发
 * {@code TextEvent}，同一段字会进来两次——那时把这个桥删掉即可。
 */
public class ToDayActivity extends GameActivity {

    @Override
    protected InputEnabledSurfaceView createSurfaceView() {
        return new ImeSurfaceView(this);
    }

    /** 实现见 {@code src/android.rs}；符号名由包名 + 类名 + 方法名拼出来。 */
    private static native void nativePushText(String text);

    private static native void nativePushBackspace(int count);

    private static class ImeSurfaceView extends GameActivity.InputEnabledSurfaceView {

        ImeSurfaceView(GameActivity activity) {
            // 继承内部类必须用限定形式：`activity` 既是外部实例，也当构造器的 Context 用
            activity.super(activity);
        }

        @Override
        public com.google.androidgamesdk.gametextinput.InputConnection onCreateInputConnection(
                EditorInfo outAttrs) {
            // 先让 GameActivity 把它那份建好（native 侧的登记也在这里做完），
            // 再交出去我们这份：只多两个钩子，其余行为完全一样
            final com.google.androidgamesdk.gametextinput.InputConnection base =
                    super.onCreateInputConnection(outAttrs);
            if (base == null) {
                // AGDK 的语义：软键盘还没被请求显示时它就返回 null（窗口刚拿到焦点时输入法
                // 会先来问一次）。照旧返回 null，下一次（`showSoftInput` 之后）才是真连接
                return null;
            }
            // `Settings(editorInfo, forwardKeyEvents)`：第二个参数照抄 AGDK 自己的算法
            // （`inputType == 0` 时把按键事件也转给界面），不然我们的连接与它那份行为会不一致
            final EditorInfo editorInfo = base.getEditorInfo();
            final Settings settings = new Settings(editorInfo, editorInfo.inputType == 0);
            final Bridge bridge = new Bridge(getContext(), this, settings);
            bridge.setListener(base.getListener());
            bridge.setSoftKeyboardActive(base.getSoftKeyboardActive(), 0);
            return bridge;
        }
    }

    private static class Bridge extends com.google.androidgamesdk.gametextinput.InputConnection {

        Bridge(Context context, View view, Settings settings) {
            super(context, view, settings);
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            if (text != null && text.length() > 0) {
                ToDayActivity.nativePushText(text.toString());
            }
            return super.commitText(text, newCursorPosition);
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            // 光标左边的删除才对应正文里的退格；右边那些（选区内删、输入法自己纠错）先不接
            if (beforeLength > 0) {
                ToDayActivity.nativePushBackspace(beforeLength);
            }
            return super.deleteSurroundingText(beforeLength, afterLength);
        }
    }
}
