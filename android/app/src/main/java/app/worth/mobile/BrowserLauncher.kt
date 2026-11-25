package app.worth.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.widget.Toast
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.net.toUri

object BrowserLauncher {
    fun open(context: Context, location: String) {
        try {
            CustomTabsIntent.Builder().setShowTitle(true).build().launchUrl(context, location.toUri())
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(context, R.string.browser_unavailable, Toast.LENGTH_LONG).show()
        }
    }
}
