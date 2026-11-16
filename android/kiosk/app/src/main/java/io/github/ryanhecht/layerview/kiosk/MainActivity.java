package io.github.ryanhecht.layerview.kiosk;

import android.app.Activity;
import android.app.admin.DevicePolicyManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.graphics.Color;
import android.os.BatteryManager;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.provider.Settings;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * Full-screen display of the layerview page served by the computer.
 * The phone reaches it at localhost over USB (`adb reverse tcp:8740 tcp:8740`).
 * As device owner it pins itself (lock task), disables the keyguard and status
 * bar, stays awake while plugged in, and registers as the home app so it
 * returns after reboots.
 */
public class MainActivity extends Activity {
    static final String DEFAULT_URL = "http://localhost:8740/";
    static final String WAITING_HTML =
            "<html><body style='margin:0;background:#000;color:#8a859c;font:18px sans-serif;"
            + "display:flex;align-items:center;justify-content:center;height:100vh'>"
            + "Waiting for layerview on the computer…</body></html>";

    private WebView web;
    private String url = DEFAULT_URL;
    private final Handler handler = new Handler(Looper.getMainLooper());
    private boolean showingError = false;
    /** Maintenance: kiosk lock released until the app is launched again without the flag. */
    private static boolean maintenance = false;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        getWindow().getDecorView().setBackgroundColor(Color.BLACK);

        String fromIntent = getIntent() != null ? getIntent().getStringExtra("url") : null;
        if (fromIntent != null) {
            getPreferences(Context.MODE_PRIVATE).edit().putString("url", fromIntent).apply();
        }
        url = getPreferences(Context.MODE_PRIVATE).getString("url", DEFAULT_URL);
        handleMaintenance(getIntent());

        web = new WebView(this);
        web.setBackgroundColor(Color.BLACK);
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setCacheMode(WebSettings.LOAD_NO_CACHE);
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onReceivedError(WebView view, WebResourceRequest req, WebResourceError err) {
                if (req.isForMainFrame()) {
                    showingError = true;
                    view.loadDataWithBaseURL(null, WAITING_HTML, "text/html", "utf-8", null);
                    handler.postDelayed(() -> load(), 3000);
                }
            }
        });
        setContentView(web);
        setupDeviceOwner();
        load();
    }

    private void load() {
        showingError = false;
        web.loadUrl(url);
    }

    private void setupDeviceOwner() {
        DevicePolicyManager dpm = getSystemService(DevicePolicyManager.class);
        if (dpm == null || !dpm.isDeviceOwnerApp(getPackageName())) return;
        ComponentName admin = new ComponentName(this, AdminReceiver.class);
        dpm.setLockTaskPackages(admin, new String[] {getPackageName()});
        dpm.setLockTaskFeatures(admin, DevicePolicyManager.LOCK_TASK_FEATURE_NONE);
        dpm.setKeyguardDisabled(admin, true);
        dpm.setStatusBarDisabled(admin, true);
        dpm.setGlobalSetting(admin, Settings.Global.STAY_ON_WHILE_PLUGGED_IN,
                String.valueOf(BatteryManager.BATTERY_PLUGGED_AC
                        | BatteryManager.BATTERY_PLUGGED_USB
                        | BatteryManager.BATTERY_PLUGGED_WIRELESS));
        IntentFilter home = new IntentFilter(Intent.ACTION_MAIN);
        home.addCategory(Intent.CATEGORY_HOME);
        home.addCategory(Intent.CATEGORY_DEFAULT);
        dpm.addPersistentPreferredActivity(admin, home, new ComponentName(this, MainActivity.class));
    }

    @Override
    protected void onResume() {
        super.onResume();
        WindowInsetsController c = getWindow().getInsetsController();
        if (c != null) {
            c.hide(WindowInsets.Type.systemBars());
            c.setSystemBarsBehavior(WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
        }
        DevicePolicyManager dpm = getSystemService(DevicePolicyManager.class);
        if (!maintenance && dpm != null && dpm.isLockTaskPermitted(getPackageName())) {
            try {
                startLockTask();
            } catch (IllegalStateException ignored) {
                // already pinned
            }
        }
        if (showingError) load();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (handleMaintenance(intent)) return;
        String u = intent.getStringExtra("url");
        if (u != null) {
            url = u;
            getPreferences(Context.MODE_PRIVATE).edit().putString("url", u).apply();
            load();
        }
    }

    /**
     * `adb shell am start -n <pkg>/.MainActivity --ez maintenance true` releases the kiosk lock
     * and opens Settings; launching with `--ez maintenance false` (or rebooting) re-locks.
     */
    private boolean handleMaintenance(Intent intent) {
        if (intent == null || !intent.hasExtra("maintenance")) return false;
        maintenance = intent.getBooleanExtra("maintenance", false);
        if (maintenance) {
            try {
                stopLockTask();
            } catch (IllegalStateException ignored) {
                // not pinned
            }
            Intent settings = new Intent(Settings.ACTION_SETTINGS);
            settings.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(settings);
        } else {
            onResume();
        }
        return true;
    }

    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        // Kiosk: ignore Back.
    }
}
